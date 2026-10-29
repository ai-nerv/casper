#![allow(clippy::expect_used)]
use crate::{
    events::*,
    mcp_client::{Hub, close, config::Server},
};
use rmcp::{
    ServerHandler,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::json;
use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

struct Peer;
impl ServerHandler for Peer {}

struct Fixture {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new() -> Self {
        let (ready, address) = mpsc::sync_channel(1);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().expect("server runtime").block_on(async {
                let service = StreamableHttpService::new(|| Ok(Peer), Arc::new(LocalSessionManager::default()), StreamableHttpServerConfig::default());
                let app = axum::Router::new().nest_service("/private", service);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("loopback bind");
                ready.send(format!("http://{}/private", listener.local_addr().expect("address"))).expect("fixture readiness");
                tokio::select! { result = axum::serve(listener, app) => result.expect("native HTTP server"), _ = stopped => {} }
            });
        });
        Self {
            url: address
                .recv_timeout(Duration::from_secs(5))
                .expect("HTTP ready"),
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
            thread.join().expect("fixture reaped");
        }
    }
}

fn take(receiver: &Receiver, outcomes: &[ServerOutcome], id: Option<Id>) -> Option<Id> {
    let mut actual = Vec::new();
    let mut identity = id;
    while let Ok(bytes) = receiver.try_recv() {
        assert!(!String::from_utf8_lossy(&bytes).contains("private"));
        let event = decode(&bytes).expect("strict event");
        let current = event.correlation.request_id.expect("attempt identity");
        assert_eq!(*identity.get_or_insert(current), current);
        assert_eq!(
            event.correlation,
            Correlation {
                request_id: Some(current),
                ..Correlation::default()
            }
        );
        let Fact::ToolServer { outcome } = event.fact else {
            panic!("native server event")
        };
        actual.push(outcome);
    }
    assert_eq!(actual, outcomes);
    identity
}

#[cfg(test)]
#[test]
fn actual_session_release() {
    use std::process::Command;
    let Ok(mode) = std::env::var("CASPER_NATIVE_OWNER_PROBE") else {
        for mode in ["close", "replace"] {
            let output = Command::new(std::env::current_exe().expect("unit executable"))
                .args([
                    "--exact",
                    "mcp_client::observing::native::actual_session_release",
                    "--nocapture",
                ])
                .env("CASPER_NATIVE_OWNER_PROBE", mode)
                .env("CASPER_JAIL", r#"{"reach":true}"#)
                .output()
                .expect("isolated native owner probe");
            assert!(
                output.status.success(),
                "native owner {mode}: {}\n{}",
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
    use ServerOutcome::*;
    let fixture = Fixture::new();
    let receiver = enable_local().expect("isolated receiver");
    let mut server: Server =
        serde_json::from_value(json!({"url":fixture.url,"timeout_secs":2})).expect("native config");
    let mut hub = Hub::new().expect("native owner");
    let held = hub
        .session("private", &server)
        .expect("real transport and discovery");
    assert_eq!(Arc::strong_count(&held), 2);
    let id = take(&receiver, &[Connecting, Connected], None);
    let replacement = if mode == "replace" {
        server
            .headers
            .insert("x-private".into(), "private-value".into());
        let next = hub
            .session("private", &server)
            .expect("replacement connection");
        assert!(!Arc::ptr_eq(&held, &next));
        let mut events = Vec::new();
        while let Ok(bytes) = receiver.try_recv() {
            events.push(decode(&bytes).expect("strict event"));
        }
        assert_eq!(events.len(), 3, "old session still has a native owner");
        let old = events.first().expect("close request");
        assert_eq!(old.correlation.request_id, id);
        assert_eq!(
            old.fact,
            Fact::ToolServer {
                outcome: CloseRequested
            }
        );
        let new_id = events.get(1).expect("connecting").correlation.request_id;
        assert!(new_id.is_some() && new_id != id);
        for (event, outcome) in events.iter().skip(1).zip([Connecting, Connected]) {
            assert_eq!(event.correlation.request_id, new_id);
            assert_eq!(event.fact, Fact::ToolServer { outcome });
        }
        Some((next, new_id))
    } else {
        let pooled = hub
            .sessions
            .remove("private")
            .expect("pooled native session");
        close(&hub.runtime, pooled);
        take(&receiver, &[CloseRequested], id);
        None
    };
    assert!(held.service.is_closed());
    assert_eq!(Arc::strong_count(&held), 1);
    let last = held.clone();
    drop(held);
    take(&receiver, &[], id);
    drop(last);
    take(&receiver, &[Disconnected], id);
    if let Some((next, next_id)) = replacement {
        assert!(!next.service.is_closed());
        drop(next);
        drop(hub);
        take(&receiver, &[CloseRequested, Disconnected], next_id);
    } else {
        drop(hub);
        take(&receiver, &[], id);
    }
    assert_eq!(loss(), Loss::default());
}
