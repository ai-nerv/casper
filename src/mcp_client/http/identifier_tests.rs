use super::auth_tests::{configured, headers};
use super::*;
use serde_json::json;
use tokio::io::AsyncWriteExt;

mod rotation;
mod sdk;
mod tracing;

fn ping() -> ClientJsonRpcMessage {
    serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"})).expect("ping")
}

#[tokio::test]
async fn restored_identifier_headers_are_sensitive_and_reject_invalid_values() {
    let endpoint = "http://127.0.0.1:1/mcp";
    let (_root, client, _access, _store, _over) = configured(endpoint).await;
    let mut request = client.inner.get(endpoint);
    for (name, kind, raw) in [
        (HEADER_SESSION_ID, Kind::Session, "session-secret-canary"),
        (HEADER_LAST_EVENT_ID, Kind::Cursor, "cursor-secret-canary"),
    ] {
        let alias = client.seal_identifier(kind, raw.into()).expect("alias");
        request = client
            .identifier_header(request, name, kind, &alias)
            .expect("header");
    }
    let request = request.build().expect("request");
    for (name, raw) in [
        (HEADER_SESSION_ID, "session-secret-canary"),
        (HEADER_LAST_EVENT_ID, "cursor-secret-canary"),
    ] {
        let header = request.headers().get(name).expect("restored header");
        assert_eq!(header.to_str().expect("ASCII"), raw);
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("secret-canary"));
    }
    assert!(!format!("{request:?}").contains("secret-canary"));
    for raw in ["secret-canary\r\nInjected: yes", "secret-canary\0"] {
        let alias = client
            .seal_identifier(Kind::Session, raw.into())
            .expect("authenticated bytes");
        let error = client
            .identifier_header(
                client.inner.get(endpoint),
                HEADER_SESSION_ID,
                Kind::Session,
                &alias,
            )
            .expect_err("invalid header refused");
        assert!(!format!("{error:?}").contains("secret-canary"));
    }
}

#[tokio::test]
async fn protected_identifiers_roundtrip_on_post_resume_and_delete_without_changing_payloads() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, _access, _store, _over) = configured(&endpoint).await;
    let server = tokio::spawn(async move {
        for step in 0..4 {
            let (socket, _) = listener.accept().await.expect("accept");
            let (mut socket, header) = headers(socket).await;
            let header = header.to_ascii_lowercase();
            assert!(header.contains("authorization: bearer access-secret-canary\r\n"));
            assert!(
                !header.contains("mcp1."),
                "SDK aliases never reach the network"
            );
            if step != 0 {
                assert!(header.contains("mcp-session-id: session-secret-canary\r\n"));
            }
            let (status, kind, extra, body) = match step {
                0 => (
                    200,
                    "application/json",
                    "Mcp-Session-Id: session-secret-canary\r\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
                ),
                1 => (
                    200,
                    "text/event-stream",
                    "",
                    "id: cursor-secret-canary\nretry: 17\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n",
                ),
                2 => {
                    assert!(header.starts_with("get "));
                    assert!(header.contains("last-event-id: cursor-secret-canary\r\n"));
                    (
                        200,
                        "text/event-stream",
                        "",
                        "id: successor-secret-canary\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":\"payload-unchanged\"}}\n\nid: \ndata: \n\n",
                    )
                }
                _ => {
                    assert!(header.starts_with("delete "));
                    (204, "application/json", "", "")
                }
            };
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: {kind}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("reply");
        }
    });
    let reply = client
        .post_message(endpoint.clone().into(), ping(), None, None, HashMap::new())
        .await
        .expect("initialize-like response");
    assert!(!format!("{reply:?}").contains("session-secret-canary"));
    let StreamableHttpPostResponse::Json(_, Some(session)) = reply else {
        panic!("session reply")
    };
    let clone = client.clone();
    let reply = clone
        .post_message(
            endpoint.clone().into(),
            ping(),
            Some(session.clone().into()),
            None,
            HashMap::new(),
        )
        .await
        .expect("post");
    let StreamableHttpPostResponse::Sse(mut stream, _) = reply else {
        panic!("SSE reply")
    };
    let event = stream.next().await.expect("event").expect("valid");
    assert!(!format!("{event:?}").contains("cursor-secret-canary"));
    assert_eq!(event.retry, Some(17));
    assert!(
        event
            .data
            .as_deref()
            .expect("notification")
            .contains("notifications/tools/list_changed")
    );
    assert!(stream.next().await.is_none());
    let mut resumed = clone
        .get_stream(
            endpoint.clone().into(),
            Some(session.clone().into()),
            event.id,
            None,
            HashMap::new(),
        )
        .await
        .expect("resume");
    let event = resumed.next().await.expect("response").expect("valid");
    assert!(!format!("{event:?}").contains("successor-secret-canary"));
    assert!(
        event
            .data
            .expect("data unchanged")
            .contains("payload-unchanged")
    );
    let reset = resumed.next().await.expect("reset").expect("valid");
    assert_eq!(reset.id.as_deref(), Some(""));
    assert!(reset.data.is_none());
    assert!(resumed.next().await.is_none());
    client
        .delete_session(endpoint.into(), session.into(), None, HashMap::new())
        .await
        .expect("delete");
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server deadline")
        .expect("server");
}

#[tokio::test]
async fn foreign_wrong_kind_and_unwrapped_identifiers_are_refused_before_http() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let (_root, client, _access, _store, _over) = configured(&endpoint).await;
    let foreign = Cipher::new(4096).expect("foreign transport");
    for id in [
        "raw-session-secret-canary".into(),
        foreign
            .seal(Kind::Session, "foreign-secret-canary")
            .expect("foreign"),
        client
            .seal_identifier(Kind::Cursor, "cursor-secret-canary".into())
            .expect("cursor"),
    ] {
        let error = client
            .get_stream(
                endpoint.clone().into(),
                Some(id.into()),
                None,
                None,
                HashMap::new(),
            )
            .await
            .err()
            .expect("refused");
        assert!(!format!("{error:?}").contains("secret-canary"));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}
