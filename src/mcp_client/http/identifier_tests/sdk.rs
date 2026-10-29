use super::*;
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::sync::{Mutex, atomic::AtomicUsize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

struct Fixture {
    endpoint: String,
    calls: Arc<AtomicUsize>,
    resumed: Arc<AtomicUsize>,
    deleted: Arc<AtomicUsize>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(super) async fn request(
    socket: tokio::net::TcpStream,
) -> (
    tokio::net::TcpStream,
    String,
    HashMap<String, String>,
    Vec<u8>,
) {
    let mut reader = BufReader::new(socket);
    let mut first = String::new();
    reader.read_line(&mut first).await.expect("request line");
    let mut header = HashMap::new();
    let mut total = first.len();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("header");
        total += line.len();
        assert!(total <= 8192);
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let (key, value) = line.split_once(':').expect("header field");
        header.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = header
        .get("content-length")
        .map(|value| value.parse::<usize>().expect("length"))
        .unwrap_or(0);
    assert!(length <= 8192);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.expect("body");
    (reader.into_inner(), first, header, body)
}

impl Fixture {
    async fn new(corrupt: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let calls = Arc::new(AtomicUsize::new(0));
        let resumed = Arc::new(AtomicUsize::new(0));
        let deleted = Arc::new(AtomicUsize::new(0));
        let (called, resumed_count, deleted_count) =
            (calls.clone(), resumed.clone(), deleted.clone());
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    Some(result) = children.join_next(), if !children.is_empty() => { result.expect("HTTP task"); },
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("accept");
                        let (called, resumed, deleted, pending) = (called.clone(), resumed_count.clone(), deleted_count.clone(), pending.clone());
                        children.spawn(async move {
                            tokio::time::timeout(Duration::from_secs(3), async {
                                let (mut socket, first, header, body) = request(socket).await;
                                let message: serde_json::Value = if body.is_empty() { serde_json::Value::Null } else { serde_json::from_slice(&body).expect("JSON") };
                                assert_eq!(header.get("authorization").map(String::as_str), Some("Bearer access-secret-canary"));
                                assert!(!header.values().any(|value| value.contains("mcp1.")));
                                let method = message.get("method").and_then(serde_json::Value::as_str);
                                if method != Some("initialize") { assert_eq!(header.get("mcp-session-id").map(String::as_str), Some("session-secret-canary")); }
                                let mut extra = "";
                                let mut truncated = false;
                                let (status, kind, body) = if first.starts_with("DELETE ") {
                                    deleted.fetch_add(1, Ordering::SeqCst);
                                    (204, "application/json", String::new())
                                } else if first.starts_with("GET ") {
                                    if let Some(cursor) = header.get("last-event-id") {
                                        let id = pending.lock().expect("pending").remove(cursor).expect("original cursor resumes its own request");
                                        resumed.fetch_add(1, Ordering::SeqCst);
                                        let reply = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"payload-unchanged"}]}});
                                        (200, "text/event-stream", format!("id: successor-secret-canary\ndata: {reply}\n\n"))
                                    } else { (405, "application/json", String::new()) }
                                } else if method == Some("initialize") {
                                    extra = "Mcp-Session-Id: session-secret-canary\r\n";
                                    (200, "application/json", json!({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"identifier-fixture","version":"1"}}}).to_string())
                                } else if method == Some("tools/call") {
                                    let sequence = called.fetch_add(1, Ordering::SeqCst);
                                    let cursor = format!("cursor-secret-canary-{sequence}");
                                    pending.lock().expect("pending").insert(cursor.clone(), message["id"].clone());
                                    let mut body = format!("id: {cursor}\nretry: 1\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\n");
                                    if corrupt { body.push_str("data: invalid-body-secret-canary\n\n"); }
                                    else { truncated = true; }
                                    (200, "text/event-stream", body)
                                } else { (202, "application/json", String::new()) };
                                socket.write_all(format!("HTTP/1.1 {status} Test\r\n{extra}Content-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()+usize::from(truncated)*17).as_bytes()).await.expect("reply");
                                socket.shutdown().await.expect("close response");
                            }).await.expect("HTTP deadline");
                        });
                    }
                }
            }
            while let Some(result) = children.join_next().await {
                result.expect("HTTP task completed");
            }
        });
        Self {
            endpoint,
            calls,
            resumed,
            deleted,
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
            .expect("server teardown")
            .expect("server");
    }
}

pub(super) async fn exercise(corrupt: bool) {
    let mut server = Fixture::new(corrupt).await;
    let (_root, client, access, _store, over) = configured(&server.endpoint).await;
    let mut config = StreamableHttpClientTransportConfig::with_uri(server.endpoint.clone())
        .max_concurrent_requests(8)
        .control_request_timeout(Duration::from_secs(2));
    config.max_sse_event_size = 4096;
    config.reinit_on_expired_session = false;
    config.retry_config = Arc::new(Patience::new(over).with_auth(Some(access.clone())));
    let service = super::super::super::Client
        .serve(StreamableHttpClientTransport::with_client(client, config))
        .await
        .expect("SDK initialize");
    let count = if corrupt { 1 } else { 8 };
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..count {
        let peer = service.peer().clone();
        calls.spawn(async move { peer.call_tool(CallToolRequestParams::new("proof")).await });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(result) = calls.join_next().await {
            let result = result.expect("caller task");
            if corrupt {
                let error = result.expect_err("malformed response fails without replay");
                assert!(!format!("{error:?}").contains("secret-canary"));
            } else {
                let result = result.expect("resumed result");
                assert!(
                    serde_json::to_string(&result)
                        .expect("reply")
                        .contains("payload-unchanged")
                );
            }
        }
    })
    .await
    .expect("SDK request deadline");
    tokio::time::timeout(Duration::from_secs(3), service.cancel())
        .await
        .expect("SDK cleanup deadline")
        .expect("SDK cleanup");
    assert_eq!(server.calls.load(Ordering::SeqCst), count);
    assert_eq!(
        server.resumed.load(Ordering::SeqCst),
        if corrupt { 0 } else { count }
    );
    assert_eq!(server.deleted.load(Ordering::SeqCst), usize::from(!corrupt));
    assert_eq!(access.stopped(), corrupt);
    server.finish().await;
}

#[tokio::test]
async fn actual_sdk_parallel_resumption_and_cleanup_keep_original_wire_ids() {
    if crate::testing::isolated(
        "mcp_client::http::identifier_tests::sdk::actual_sdk_parallel_resumption_and_cleanup_keep_original_wire_ids",
    ) {
        return;
    }
    let captured = super::tracing::Capture::install();
    exercise(false).await;
    captured.verify();
}

#[tokio::test]
async fn actual_sdk_malformed_stream_stops_without_replay_or_secret_errors() {
    if crate::testing::isolated(
        "mcp_client::http::identifier_tests::sdk::actual_sdk_malformed_stream_stops_without_replay_or_secret_errors",
    ) {
        return;
    }
    let captured = super::tracing::Capture::install();
    exercise(true).await;
    captured.verify();
}
