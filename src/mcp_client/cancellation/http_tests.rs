use super::*;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler, service::RequestContext};
use std::{os::unix::net::UnixStream, sync::mpsc, thread};

struct Peer {
    id: u64,
    entered: mpsc::SyncSender<(u64, String)>,
    release: Arc<tokio::sync::Notify>,
}

impl ServerHandler for Peer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: serde_json::from_value(json!([
                {"name":"wait","description":"wait","inputSchema":{"type":"object"}},
                {"name":"echo","description":"echo","inputSchema":{"type":"object"}}
            ]))
            .expect("schemas"),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let role = request
            .arguments
            .as_ref()
            .and_then(|fields| fields.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("echo")
            .to_owned();
        self.entered
            .try_send((self.id, role))
            .expect("bounded request observations");
        if request.name == "wait" {
            tokio::select! {
                () = context.ct.cancelled() => return Ok(CallToolResult::error(vec![ContentBlock::text("cancelled")]).into()),
                () = self.release.notified() => (),
            }
        }
        Ok(CallToolResult::success(vec![ContentBlock::text("useful-response")]).into())
    }
}

struct Fixture {
    configuration: Value,
    entered: mpsc::Receiver<(u64, String)>,
    release: Arc<tokio::sync::Notify>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new() -> Self {
        let (started, entered) = mpsc::sync_channel(16);
        let (send, ready) = mpsc::sync_channel(1);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let released = release.clone();
        let thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fixture runtime")
                .block_on(async {
                    let sequence = std::sync::atomic::AtomicU64::new(0);
                    let service = StreamableHttpService::new(
                        move || {
                            Ok(Peer {
                                id: sequence.fetch_add(1, Ordering::Relaxed),
                                entered: started.clone(),
                                release: released.clone(),
                            })
                        },
                        Arc::new(LocalSessionManager::default()),
                        StreamableHttpServerConfig::default(),
                    );
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                        .await
                        .expect("loopback");
                    send.send(format!(
                        "http://{}/mcp",
                        listener.local_addr().expect("address")
                    ))
                    .expect("ready");
                    let app = axum::Router::new().nest_service("/mcp", service);
                    tokio::select! {
                        result = axum::serve(listener, app) => result.expect("HTTP fixture"),
                        _ = stopped => (),
                    }
                });
        });
        let url = ready
            .recv_timeout(Duration::from_secs(2))
            .expect("ready fixture");
        Self {
            configuration: json!({
                "target":{"url":url,"needs":"reach","timeout_secs":10},
                "healthy":{"url":url,"needs":"reach","timeout_secs":10}
            }),
            entered,
            release,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn started(&self, role: &str) -> u64 {
        let (id, actual) = self
            .entered
            .recv_timeout(Duration::from_secs(2))
            .expect("actual HTTP request");
        assert_eq!(actual, role);
        id
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("server joined");
        }
    }
}

fn pending(
    configuration: &Value,
    alias: &str,
    role: &str,
) -> (UnixStream, mpsc::Receiver<Ran>, thread::JoinHandle<()>) {
    let (ours, peer) = UnixStream::pair().expect("caller");
    let configuration = configuration.clone();
    let tool = format!("mcp__{alias}__wait");
    let args = json!({"role":role});
    let (send, result) = mpsc::sync_channel(1);
    let task = thread::spawn(move || {
        let result =
            crate::cancelling::with_peer(&ours, || call(Some(&configuration), &tool, &args));
        send.send(result).expect("call result");
    });
    (peer, result, task)
}

fn pooled(alias: &str) -> Arc<Session> {
    HUB.lock()
        .expect("pool")
        .as_ref()
        .expect("initialized pool")
        .sessions
        .get(alias)
        .expect("pooled session")
        .clone()
}

#[cfg(test)]
#[test]
fn actual_http_disconnect_cancels_shared_calls_preserves_other_session_and_recovers() {
    const CASE: &str = "mcp_client::cancellation::http_tests::actual_http_disconnect_cancels_shared_calls_preserves_other_session_and_recovers";
    if std::env::var_os("CASPER_HTTP_CANCEL_PROBE").is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", CASE, "--nocapture"])
            .env("CASPER_HTTP_CANCEL_PROBE", "1")
            .env("CASPER_JAIL", r#"{"reach":true}"#)
            .output()
            .expect("isolated HTTP cancellation");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
        );
        return;
    }
    crate::web::crypto();
    let fixture = Fixture::new();
    let (_healthy_peer, healthy_result, healthy_task) =
        pending(&fixture.configuration, "healthy", "healthy");
    let healthy_id = fixture.started("healthy");
    let healthy = pooled("healthy");
    let (first_peer, first_result, first_task) = pending(&fixture.configuration, "target", "first");
    let first_id = fixture.started("first");
    let (_second_peer, second_result, second_task) =
        pending(&fixture.configuration, "target", "second");
    assert_eq!(fixture.started("second"), first_id);
    assert_ne!(first_id, healthy_id);
    let target = pooled("target");
    drop(first_peer);
    let first = first_result
        .recv_timeout(Duration::from_secs(2))
        .expect("caller cancellation");
    assert!(
        first.failed && first.said.contains(DISCONNECTED),
        "{}",
        first.said
    );
    let second = second_result
        .recv_timeout(Duration::from_secs(2))
        .expect("shared session cancellation");
    assert!(second.failed, "{}", second.said);
    first_task.join().expect("first caller joined");
    second_task.join().expect("second caller joined");
    assert!(target.stopped.load(Ordering::Acquire) && target.service.is_closed());
    assert!(!healthy.stopped.load(Ordering::Acquire) && !healthy.service.is_closed());
    assert!(matches!(
        healthy_result.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(Arc::ptr_eq(&healthy, &pooled("healthy")));
    fixture.release.notify_waiters();
    let healthy_reply = healthy_result
        .recv_timeout(Duration::from_secs(2))
        .expect("unrelated useful work");
    assert!(
        !healthy_reply.failed && healthy_reply.said.contains("useful-response"),
        "{}",
        healthy_reply.said
    );
    healthy_task.join().expect("healthy caller joined");
    let recovered = call(
        Some(&fixture.configuration),
        "mcp__target__echo",
        &json!({}),
    );
    assert!(
        !recovered.failed && recovered.said.contains("useful-response"),
        "{}",
        recovered.said
    );
    assert_ne!(fixture.started("echo"), first_id);
    assert!(!Arc::ptr_eq(&target, &pooled("target")));
    assert!(Arc::ptr_eq(&healthy, &pooled("healthy")));
    assert!(matches!(
        fixture.entered.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    shutdown();
}
