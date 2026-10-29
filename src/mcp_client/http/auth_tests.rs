use super::*;
use crate::{
    mcp_client::oauth::{Access, Config, Credential, Store},
    scratch::Scratch,
};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub(super) async fn configured(
    endpoint: &str,
) -> (Scratch, Bounded, Arc<Access>, Arc<Store>, Arc<AtomicBool>) {
    let root = Scratch::new("oauth", "http");
    let origin = url::Url::parse(endpoint)
        .expect("URL")
        .origin()
        .ascii_serialization();
    let config: Config = serde_json::from_value(json!({"issuer":origin,"resource":endpoint,"client_id":"native","private_origins":[origin]})).expect("config");
    let store = Arc::new(
        Store::at(&root.join("store"), config.bind(endpoint).expect("binding")).expect("store"),
    );
    store
        .replace(
            0,
            Credential {
                access_token: "access-secret-canary".into(),
                refresh_token: None,
                expires_at: None,
                scopes: vec![],
            },
        )
        .expect("credential");
    let access = Access::attach(&config, endpoint, store.clone())
        .await
        .expect("access");
    crate::web::crypto();
    let over = Arc::new(AtomicBool::new(false));
    let client = Bounded::new(
        super::super::oauth::client(&config, endpoint)
            .await
            .expect("client")
            .1,
        4096,
        over.clone(),
    )
    .with_auth(Some(access.clone()))
    .expect("identifier protection");
    (root, client, access, store, over)
}

pub(super) async fn headers(socket: tokio::net::TcpStream) -> (tokio::net::TcpStream, String) {
    let mut reader = BufReader::new(socket);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("line");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        headers.push_str(&line);
        assert!(headers.len() < 8192);
    }
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("length"))
        })
        .unwrap_or(0);
    assert!(length <= 8192);
    reader.read_exact(&mut vec![0; length]).await.expect("body");
    (reader.into_inner(), headers)
}

#[tokio::test]
async fn get_delete_and_external_logout_use_live_bound_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, access, store, over) = configured(&endpoint).await;
    let server = tokio::spawn(async move {
        for method in ["GET", "DELETE"] {
            let (socket, _) = listener.accept().await.expect("accept");
            let (mut socket, received) = headers(socket).await;
            assert!(received.starts_with(method));
            assert!(
                received
                    .to_ascii_lowercase()
                    .contains("authorization: bearer access-secret-canary\r\n")
            );
            let reply: &[u8] = if method == "GET" {
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\n\r\n"
            } else {
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
            };
            socket.write_all(reply).await.expect("reply");
        }
    });
    let stream = client
        .get_stream(
            endpoint.clone().into(),
            Some(
                client
                    .seal_identifier(Kind::Session, "session".into())
                    .expect("session")
                    .into(),
            ),
            None,
            None,
            HashMap::new(),
        )
        .await
        .expect("stream");
    drop(stream);
    client
        .delete_session(
            endpoint.clone().into(),
            client
                .seal_identifier(Kind::Session, "session".into())
                .expect("session")
                .into(),
            None,
            HashMap::new(),
        )
        .await
        .expect("delete");
    server.await.expect("server");
    store.logout().expect("external process tombstone");
    let error = client
        .get_stream(endpoint.into(), None, None, None, HashMap::new())
        .await
        .err()
        .expect("stale bearer rejected before connect");
    assert!(!error.to_string().contains("secret-canary"));
    assert!(access.stopped());
    assert!(!over.load(Ordering::Relaxed));
    assert!(
        Patience::new(over)
            .with_auth(Some(access))
            .retry(0)
            .is_none()
    );
}

#[tokio::test]
async fn authentication_failure_is_sanitized_and_stops_sse_reconnection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, access, _store, over) = configured(&endpoint).await;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let (mut socket, _) = headers(socket).await;
        socket
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 20\r\n\r\naccess-secret-canary!",
            )
            .await
            .expect("refusal");
    });
    let error = client
        .get_stream(endpoint.into(), None, None, None, HashMap::new())
        .await
        .err()
        .expect("unauthorized");
    assert!(error.to_string().contains("authentication_required"));
    assert!(!error.to_string().contains("secret-canary"));
    assert!(
        Patience::new(over.clone())
            .with_auth(Some(access))
            .retry(1)
            .is_none()
    );
    assert!(!over.load(Ordering::Relaxed));
    server.await.expect("server");
}

#[tokio::test]
async fn local_logout_cancels_a_stalled_header_read_without_waiting_for_hub() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, access, store, _over) = configured(&endpoint).await;
    let (started, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let (mut socket, _) = headers(socket).await;
        started.send(()).expect("started");
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.expect("peer closed"), 0);
    });
    let request = tokio::spawn(async move {
        client
            .get_stream(endpoint.into(), None, None, None, HashMap::new())
            .await
    });
    ready.await.expect("ready");
    store.logout().expect("tombstone");
    access.stop();
    let result = tokio::time::timeout(Duration::from_millis(500), request)
        .await
        .expect("cancel deadline")
        .expect("task");
    assert!(result.is_err());
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("socket close deadline")
        .expect("server");
}

#[tokio::test]
async fn external_logout_ends_an_already_open_sse_stream_and_prevents_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, access, store, over) = configured(&endpoint).await;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let (mut socket, _) = headers(socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/ping\"}\n\n").await.expect("stream");
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.expect("closed"), 0);
    });
    let mut stream = client
        .get_stream(endpoint.into(), None, None, None, HashMap::new())
        .await
        .expect("stream");
    assert!(stream.next().await.is_some());
    store.logout().expect("logout");
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("stream ended after external tombstone")
            .is_none()
    );
    drop(stream);
    assert!(
        Patience::new(over)
            .with_auth(Some(access))
            .retry(0)
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("socket deadline")
        .expect("server");
}

#[tokio::test]
async fn authenticated_failures_do_not_reflect_urls_headers_or_payloads() {
    for (method, reply) in [
        ("GET", ""),
        ("POST", ""),
        ("DELETE", ""),
        ("GET", "HTTP/1.1 500 Failure\r\nContent-Length: 0\r\n\r\n"),
        (
            "DELETE",
            "HTTP/1.1 500 Failure\r\nContent-Length: 0\r\n\r\n",
        ),
        (
            "GET",
            "HTTP/1.1 200 OK\r\nContent-Type: access-secret-canary\r\nContent-Length: 0\r\n\r\n",
        ),
        (
            "POST",
            "HTTP/1.1 200 OK\r\nContent-Type: access-secret-canary\r\nContent-Length: 0\r\n\r\n",
        ),
        (
            "POST",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 22\r\n\r\n\"access-secret-canary!\"",
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let endpoint = format!(
            "http://{}/mcp?access-secret-canary",
            listener.local_addr().expect("address")
        );
        let (_root, client, _access, _store, _over) = configured(&endpoint).await;
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let (mut socket, received) = headers(socket).await;
            assert!(received.starts_with(method));
            socket.write_all(reply.as_bytes()).await.expect("reply");
        });
        let error = tokio::time::timeout(Duration::from_secs(3), async {
            match method {
                "GET" => client
                    .get_stream(endpoint.into(), None, None, None, HashMap::new())
                    .await
                    .err(),
                "DELETE" => client
                    .delete_session(
                        endpoint.into(),
                        client
                            .seal_identifier(Kind::Session, "session".into())
                            .expect("session")
                            .into(),
                        None,
                        HashMap::new(),
                    )
                    .await
                    .err(),
                _ => client
                    .post_message(
                        endpoint.into(),
                        serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
                            .expect("ping"),
                        None,
                        None,
                        HashMap::new(),
                    )
                    .await
                    .err(),
            }
            .expect("refused")
        })
        .await
        .expect("request deadline");
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            assert!(
                !diagnostic.contains("secret-canary"),
                "{method}: {diagnostic}"
            );
            assert!(diagnostic.contains("authenticated MCP"), "{diagnostic}");
        }
        server.await.expect("server");
    }
}

#[tokio::test]
async fn authenticated_sse_refuses_invalid_data_before_sdk_parsing() {
    for method in ["GET", "POST"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let (_root, client, access, _store, over) = configured(&endpoint).await;
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let (mut socket, _) = headers(socket).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nid: access-secret-canary\ndata: \"access-secret-canary\"\n\n").await.expect("reply");
        });
        let mut stream = if method == "GET" {
            client
                .get_stream(endpoint.into(), None, None, None, HashMap::new())
                .await
                .expect("stream")
        } else {
            match client
                .post_message(
                    endpoint.into(),
                    serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
                        .expect("ping"),
                    None,
                    None,
                    HashMap::new(),
                )
                .await
                .expect("post")
            {
                StreamableHttpPostResponse::Sse(stream, _) => stream,
                _ => panic!("SSE reply"),
            }
        };
        let error = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("deadline")
            .expect("event")
            .expect_err("invalid data");
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            assert!(!diagnostic.contains("secret-canary"));
            assert!(diagnostic.contains("not JSON-RPC"));
        }
        assert!(stream.next().await.is_none());
        assert!(access.stopped());
        assert!(
            Patience::new(over)
                .with_auth(Some(access))
                .retry(0)
                .is_none()
        );
        server.await.expect("server");
    }
}

#[tokio::test]
async fn authenticated_sse_preserves_valid_data_cursors_and_priming() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, access, _store, over) = configured(&endpoint).await;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let (mut socket, received) = headers(socket).await;
        assert!(received.contains("last-event-id: previous"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nid: priming\nretry: 17\ndata: \n\nevent: ignored\nid: control\ndata: arbitrary-control-payload\n\nid: reply\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":\"access-secret-canary\"}}\n\n").await.expect("reply");
    });
    let mut stream = client
        .get_stream(
            endpoint.into(),
            None,
            Some(
                client
                    .seal_identifier(Kind::Cursor, "previous".into())
                    .expect("cursor"),
            ),
            None,
            HashMap::new(),
        )
        .await
        .expect("stream");
    let first = stream.next().await.expect("priming").expect("valid");
    assert_eq!(
        client
            .open_identifier(Kind::Cursor, first.id.as_deref().expect("opaque cursor"))
            .expect("original"),
        "priming"
    );
    assert_eq!(first.retry, Some(17));
    assert!(first.data.is_none());
    let control = stream.next().await.expect("control").expect("valid");
    assert_eq!(
        client
            .open_identifier(Kind::Cursor, control.id.as_deref().expect("opaque cursor"))
            .expect("original"),
        "control"
    );
    assert_eq!(control.event.as_deref(), Some("ignored"));
    assert!(control.data.is_none());
    let reply = stream.next().await.expect("reply").expect("valid");
    assert_eq!(
        client
            .open_identifier(Kind::Cursor, reply.id.as_deref().expect("opaque cursor"))
            .expect("original"),
        "reply"
    );
    assert!(
        reply
            .data
            .expect("unchanged payload")
            .contains("access-secret-canary")
    );
    assert!(stream.next().await.is_none());
    assert!(!access.stopped());
    assert!(
        Patience::new(over)
            .with_auth(Some(access))
            .retry(0)
            .is_some()
    );
    server.await.expect("server");
}
