use super::*;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use std::{os::unix::net::UnixStream, thread};

struct Peer;
impl rmcp::ServerHandler for Peer {}

struct Fixture {
    server: Server,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new() -> Self {
        let (send, ready) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fixture runtime")
                .block_on(async {
                    let service = StreamableHttpService::new(
                        || Ok(Peer),
                        Arc::new(LocalSessionManager::default()),
                        StreamableHttpServerConfig::default(),
                    );
                    let app = axum::Router::new().nest_service("/mcp", service);
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                        .await
                        .expect("loopback");
                    send.send(format!(
                        "http://{}/mcp",
                        listener.local_addr().expect("address")
                    ))
                    .expect("ready");
                    tokio::select! {
                        result = axum::serve(listener, app) => result.expect("fixture server"),
                        _ = stopped => (),
                    }
                });
        });
        let url = ready
            .recv_timeout(Duration::from_secs(2))
            .expect("fixture readiness");
        Self {
            server: serde_json::from_value(json!({"url":url,"timeout_secs":2})).expect("config"),
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fixture joined");
        }
    }
}

#[cfg(test)]
#[test]
fn cancelled_publication_retires_only_new_session_instances() {
    const CASE: &str =
        "mcp_client::admission::tests::cancelled_publication_retires_only_new_session_instances";
    let Ok(mode) = std::env::var("CASPER_ADMISSION_PROBE") else {
        for mode in ["new", "replace", "reuse", "generation"] {
            let output = std::process::Command::new(std::env::current_exe().expect("test image"))
                .args(["--exact", CASE, "--nocapture"])
                .env("CASPER_ADMISSION_PROBE", mode)
                .env("CASPER_JAIL", r#"{"reach":true}"#)
                .output()
                .expect("isolated case");
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
            );
        }
        return;
    };
    crate::web::crypto();
    let fixture = Fixture::new();
    let mut hub = Hub::new().expect("hub");
    let healthy = hub
        .session("healthy", &fixture.server)
        .expect("unrelated session");
    let prior = matches!(mode.as_str(), "replace" | "reuse").then(|| {
        hub.session("target", &fixture.server)
            .expect("prior session")
    });
    let mut server = fixture.server.clone();
    if mode == "replace" {
        server
            .headers
            .insert("x-generation".into(), "replacement".into());
    }
    let (ours, peer) = UnixStream::pair().expect("caller");
    let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
    let mut peer = Some(peer);
    let scope = hub.scope.clone();
    let mut selected = None;
    let result = hub.admit(&caller, &scope, |hub| {
        let session = hub.session("target", &server)?;
        assert!(
            hub.sessions
                .get("target")
                .is_some_and(|pooled| Arc::ptr_eq(pooled, &session))
        );
        selected = Some(session);
        if mode == "generation" {
            drop(lifecycle::closing());
        } else {
            peer.take();
        }
        Ok(())
    });
    let error = result.expect_err("cancelled publication");
    if mode == "generation" {
        assert!(error.contains("pool stopping"), "{error}");
    }
    let selected = selected.expect("published session");
    if mode == "reuse" {
        assert!(
            hub.sessions
                .get("target")
                .is_some_and(|pooled| Arc::ptr_eq(pooled, &selected))
        );
        assert!(!selected.stopped.load(Ordering::Acquire));
        assert!(!selected.service.is_closed());
    } else {
        assert!(!hub.sessions.contains_key("target"));
        assert!(selected.stopped.load(Ordering::Acquire));
        assert!(selected.service.is_closed());
    }
    assert!(
        hub.sessions
            .get("healthy")
            .is_some_and(|pooled| Arc::ptr_eq(pooled, &healthy))
    );
    assert!(!healthy.stopped.load(Ordering::Acquire));
    assert!(!healthy.service.is_closed());
    if mode != "generation" {
        let fresh = hub
            .session("target", &server)
            .expect("later explicit recovery");
        assert_eq!(Arc::ptr_eq(&fresh, &selected), mode == "reuse");
        let unchanged = hub
            .session("healthy", &fixture.server)
            .expect("unrelated reuse");
        assert!(Arc::ptr_eq(&unchanged, &healthy));
    }
    if mode == "replace" {
        assert!(prior.expect("prior").service.is_closed());
    }
}
