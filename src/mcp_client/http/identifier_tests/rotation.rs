use super::*;
use crate::{
    mcp_client::oauth::{Access, Config, Credential, Store},
    scratch::Scratch,
};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize},
};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[derive(Default)]
struct Counts {
    tokens: AtomicUsize,
    initialized: AtomicUsize,
    calls: AtomicUsize,
    resumes: AtomicUsize,
    deletes: AtomicUsize,
    expires: AtomicU64,
    pending: Mutex<HashMap<String, serde_json::Value>>,
}

struct Server {
    origin: String,
    counts: Arc<Counts>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Server {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let base = origin.clone();
        let counts = Arc::new(Counts::default());
        let state = counts.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    Some(result) = children.join_next(), if !children.is_empty() => { result.expect("HTTP worker"); },
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("accept");
                        let (base, state) = (base.clone(), state.clone());
                        children.spawn(async move {
                            tokio::time::timeout(Duration::from_secs(12), respond(socket, &base, &state)).await.expect("HTTP deadline");
                        });
                    }
                }
            }
            while let Some(result) = children.join_next().await {
                result.expect("HTTP worker joined");
            }
        });
        Self {
            origin,
            counts,
            stop: Some(stop),
            task: Some(task),
        }
    }

    async fn finish(&mut self) {
        self.stop
            .take()
            .expect("stop")
            .send(())
            .expect("server active");
        tokio::time::timeout(Duration::from_secs(3), self.task.take().expect("task"))
            .await
            .expect("teardown deadline")
            .expect("server joined");
    }
}

async fn respond(socket: tokio::net::TcpStream, base: &str, state: &Counts) {
    let (mut socket, first, headers, bytes) = super::sdk::request(socket).await;
    let path = first.split_whitespace().nth(1).expect("path");
    assert!(!headers.values().any(|value| value.contains("mcp1.")));
    let token = headers.get("authorization").map(String::as_str);
    let mut extra = String::new();
    let mut partial = false;
    let (status, kind, body) = if token.is_none() {
        assert!(!headers.contains_key("mcp-session-id"));
        assert!(!headers.contains_key("last-event-id"));
        let body = match path {
            "/mcp" => {
                extra =
                    format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/metadata\"\r\n");
                serde_json::Value::Null
            }
            "/metadata" => json!({"resource":format!("{base}/mcp"),"authorization_servers":[base]}),
            "/.well-known/oauth-authorization-server" => {
                json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"]})
            }
            "/token" => {
                let sequence = state.tokens.fetch_add(1, Ordering::SeqCst);
                assert!(
                    sequence < 2,
                    "refresh is single-flight, not per resumed stream"
                );
                let form: HashMap<_, _> =
                    url::form_urlencoded::parse(&bytes).into_owned().collect();
                assert_eq!(
                    form.get("grant_type").map(String::as_str),
                    Some("refresh_token")
                );
                assert_eq!(form.get("client_id").map(String::as_str), Some("native"));
                assert_eq!(form.get("resource"), Some(&format!("{base}/mcp")));
                assert_eq!(
                    form.get("refresh_token").map(String::as_str),
                    Some(if sequence == 0 {
                        "initial-refresh-secret-canary"
                    } else {
                        "rotated-refresh-secret-canary"
                    })
                );
                let expires = if sequence == 0 { 5 } else { 3600 };
                state.expires.store(now() + expires, Ordering::SeqCst);
                json!({"access_token":if sequence == 0 {"initial-access-secret-canary"} else {"rotated-access-secret-canary"},"refresh_token":"rotated-refresh-secret-canary","expires_in":expires,"token_type":"Bearer"})
            }
            _ => panic!("unexpected control request"),
        };
        (
            if path == "/mcp" { 401 } else { 200 },
            "application/json",
            body.to_string(),
        )
    } else {
        assert_eq!(path, "/mcp");
        let message: serde_json::Value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("JSON")
        };
        let method = message.get("method").and_then(serde_json::Value::as_str);
        if method != Some("initialize") {
            assert_eq!(
                headers.get("mcp-session-id").map(String::as_str),
                Some("session-rotation-secret-canary")
            );
        }
        if first.starts_with("DELETE ") {
            assert_eq!(token, Some("Bearer rotated-access-secret-canary"));
            state.deletes.fetch_add(1, Ordering::SeqCst);
            (204, "application/json", String::new())
        } else if first.starts_with("GET ") {
            if let Some(cursor) = headers.get("last-event-id") {
                assert_eq!(token, Some("Bearer rotated-access-secret-canary"));
                let id = state
                    .pending
                    .lock()
                    .expect("pending")
                    .remove(cursor)
                    .expect("original cursor");
                state.resumes.fetch_add(1, Ordering::SeqCst);
                let reply = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"payload-unchanged"}]}});
                (200, "text/event-stream", format!("data: {reply}\n\n"))
            } else {
                (405, "application/json", String::new())
            }
        } else if method == Some("initialize") {
            assert_eq!(token, Some("Bearer initial-access-secret-canary"));
            state.initialized.fetch_add(1, Ordering::SeqCst);
            extra = "Mcp-Session-Id: session-rotation-secret-canary\r\n".into();
            (200, "application/json", json!({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"rotation-fixture","version":"1"}}}).to_string())
        } else if method == Some("tools/call") {
            assert_eq!(token, Some("Bearer initial-access-secret-canary"));
            let sequence = state.calls.fetch_add(1, Ordering::SeqCst);
            assert!(sequence < 2, "interrupted tool calls must not be replayed");
            let cursor = format!("rotation-cursor-secret-canary-{sequence}");
            state
                .pending
                .lock()
                .expect("pending")
                .insert(cursor.clone(), message["id"].clone());
            partial = true;
            (
                200,
                "text/event-stream",
                format!(
                    "id: {cursor}\nretry: 1\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\n"
                ),
            )
        } else {
            (202, "application/json", String::new())
        }
    };
    socket.write_all(format!("HTTP/1.1 {status} Test\r\n{extra}Content-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len() + usize::from(partial) * 17).as_bytes()).await.expect("response");
    if partial {
        let expiry = state.expires.load(Ordering::SeqCst);
        while now() < expiry {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    socket.shutdown().await.expect("close");
}

#[tokio::test]
async fn actual_sdk_parallel_resume_refreshes_once_without_reinitializing_or_replaying() {
    if crate::testing::isolated(
        "mcp_client::http::identifier_tests::rotation::actual_sdk_parallel_resume_refreshes_once_without_reinitializing_or_replaying",
    ) {
        return;
    }
    let captured = super::tracing::Capture::install();
    let mut server = Server::new().await;
    let endpoint = format!("{}/mcp", server.origin);
    let config: Config = serde_json::from_value(json!({"issuer":server.origin,"resource":endpoint,"client_id":"native","private_origins":[server.origin],"timeout_secs":3})).expect("config");
    let root = Scratch::new("oauth", "resume-rotation");
    let store = Arc::new(
        Store::at(
            &root.join("store"),
            config.bind(&endpoint).expect("binding"),
        )
        .expect("store"),
    );
    store
        .replace(
            0,
            Credential {
                access_token: "expired-access-secret-canary".into(),
                refresh_token: Some("initial-refresh-secret-canary".into()),
                expires_at: Some(1),
                scopes: vec![],
            },
        )
        .expect("seed");
    let access = Access::attach(&config, &endpoint, store.clone())
        .await
        .expect("access");
    let over = Arc::new(AtomicBool::new(false));
    let client = Bounded::new(
        crate::mcp_client::oauth::client(&config, &endpoint)
            .await
            .expect("client")
            .1,
        4096,
        over.clone(),
    )
    .with_auth(Some(access.clone()))
    .expect("identifier cipher");
    let mut transport = StreamableHttpClientTransportConfig::with_uri(endpoint)
        .max_concurrent_requests(2)
        .control_request_timeout(Duration::from_secs(3));
    transport.reinit_on_expired_session = false;
    transport.retry_config = Arc::new(Patience::new(over).with_auth(Some(access.clone())));
    let service = super::super::super::Client
        .serve(StreamableHttpClientTransport::with_client(
            client, transport,
        ))
        .await
        .expect("initialize");
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let peer = service.peer().clone();
        calls.spawn(async move { peer.call_tool(CallToolRequestParams::new("proof")).await });
    }
    tokio::time::timeout(Duration::from_secs(12), async {
        while let Some(result) = calls.join_next().await {
            let reply = result.expect("caller joined").expect("resumed call");
            assert!(
                serde_json::to_string(&reply)
                    .expect("payload")
                    .contains("payload-unchanged")
            );
        }
    })
    .await
    .expect("resumption deadline");
    tokio::time::timeout(Duration::from_secs(3), service.cancel())
        .await
        .expect("cleanup deadline")
        .expect("cleanup");
    for (counter, expected) in [
        (&server.counts.tokens, 2),
        (&server.counts.initialized, 1),
        (&server.counts.calls, 2),
        (&server.counts.resumes, 2),
        (&server.counts.deletes, 1),
    ] {
        assert_eq!(counter.load(Ordering::SeqCst), expected);
    }
    assert!(server.counts.pending.lock().expect("pending").is_empty());
    assert!(!access.stopped());
    let state = store.snapshot().expect("committed rotation");
    assert_eq!(state.epoch, 1);
    assert_eq!(state.generation, 5);
    let credential = state.credential.expect("credential");
    assert_eq!(credential.access_token, "rotated-access-secret-canary");
    assert_eq!(
        credential.refresh_token.as_deref(),
        Some("rotated-refresh-secret-canary")
    );
    server.finish().await;
    captured.verify();
}
