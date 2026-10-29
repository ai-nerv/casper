#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::{events::*, mcp_client, scratch::Scratch};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};

#[path = "event_servers/fixture.rs"]
mod fixture;
#[path = "event_servers/http.rs"]
mod http;
#[path = "support/native_namespace.rs"]
mod native_namespace;
#[path = "event_servers/racing.rs"]
mod racing;

fn drain(receiver: &Receiver) -> Vec<Event> {
    let mut result = Vec::new();
    while let Ok(bytes) = receiver.try_recv() {
        assert!(bytes.len() <= MAX_EVENT_BYTES);
        let text = std::str::from_utf8(&bytes).expect("JSON");
        for private in ["private", "127.0.0.1", "Bearer", "CASPER", "HOME", "file:"] {
            assert!(!text.contains(private), "event leaked {private}");
        }
        let event = decode(&bytes).expect("strict event");
        let id = event.correlation.request_id.expect("local attempt ID");
        assert_eq!(
            event.correlation,
            Correlation {
                request_id: Some(id),
                ..Correlation::default()
            }
        );
        result.push(event);
    }
    result
}

fn check(receiver: &Receiver, expected: &[ServerOutcome]) -> Vec<Event> {
    let events = drain(receiver);
    let actual: Vec<_> = events
        .iter()
        .map(|event| {
            let Fact::ToolServer { outcome } = event.fact else {
                panic!("server fact")
            };
            outcome
        })
        .collect();
    assert_eq!(actual, expected);
    if let Some(first) = events.first() {
        assert!(
            events
                .iter()
                .all(|event| event.correlation == first.correlation)
        );
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }
    events
}

fn configuration(mode: &str) -> Value {
    json!({"private":{"command":std::env::current_dir().expect("workspace").join("private-server"),
        "args":["--server", mode],"env":{"PRIVATE_SECRET":"private-environment"},"needs":"read","timeout_secs":1}})
}

fn useful(config: &Value) -> u32 {
    let cards = mcp_client::cards(Some(config)).expect("discovery");
    assert_eq!(cards.len(), 1, "actual server discovered");
    let ran = mcp_client::call(
        Some(config),
        "mcp__private__echo",
        &json!({"text":"private-argument"}),
    );
    assert!(!ran.failed, "{}", ran.said);
    let reply: Value = serde_json::from_str(&ran.said).expect("MCP reply");
    assert_eq!(reply["structuredContent"]["echo"], "private-argument");
    let pid: u32 = reply["structuredContent"]["pid"]
        .as_u64()
        .expect("server PID")
        .try_into()
        .expect("PID range");
    fixture::host_pid_at(
        pid,
        std::path::Path::new(
            config["private"]["command"]
                .as_str()
                .expect("selected executable"),
        ),
    )
}

fn reaped(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "server {pid} must be reaped");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn probe(mode: &str) {
    if mode.starts_with("race-") {
        racing::probe(mode);
        return;
    }
    use ServerOutcome::*;
    let mut config = configuration("ready");
    if mode == "disabled" || mode == "late" {
        let pid = useful(&config);
        if mode == "disabled" {
            mcp_client::shutdown();
            reaped(pid);
        }
        let receiver = enable_local().expect("late activation");
        if mode == "late" {
            assert_eq!(useful(&config), pid);
            mcp_client::shutdown();
            reaped(pid);
        }
        check(&receiver, &[]);
        assert_eq!(loss(), Loss::default());
        return;
    }
    let receiver = enable_local().expect("isolated activation");
    if mode.starts_with("http") {
        http::probe(mode, &receiver);
        return;
    }
    if mode == "closed" {
        drop(receiver);
        let pid = useful(&config);
        mcp_client::shutdown();
        reaped(pid);
        assert_eq!(
            loss(),
            Loss {
                closed: 4,
                ..Loss::default()
            }
        );
        return;
    }
    if mode == "full" {
        for _ in 0..QUEUED_EVENTS {
            assert_eq!(
                emit(|| (
                    Correlation::default(),
                    Fact::ToolServer { outcome: Unknown }
                )),
                Emitted::Queued
            );
        }
        let pid = useful(&config);
        mcp_client::shutdown();
        reaped(pid);
        assert_eq!(
            loss(),
            Loss {
                full: 4,
                ..Loss::default()
            }
        );
        for _ in 0..QUEUED_EVENTS {
            receiver.try_recv().expect("bounded original queue");
        }
        assert!(receiver.try_recv().is_err());
        let pid = useful(&config);
        mcp_client::shutdown();
        reaped(pid);
        check(
            &receiver,
            &[Connecting, Connected, CloseRequested, Disconnected],
        );
        return;
    }
    match mode {
        "refused" => config["private"]["network"] = json!(true),
        "spawn" => config["private"]["command"] = json!("/missing-private-command"),
        "discovery" | "timeout" => config = configuration(mode),
        "off" => config["private"]["off"] = json!(true),
        "invalid" => config["private"]["private-extra"] = json!(true),
        _ => {}
    }
    if ["refused", "spawn", "discovery", "timeout", "off", "invalid"].contains(&mode) {
        let cards = mcp_client::cards(Some(&config));
        if mode == "invalid" {
            assert!(cards.is_err());
        } else {
            assert!(cards.expect("catalog").is_empty());
        }
        mcp_client::shutdown();
        check(
            &receiver,
            match mode {
                "off" | "invalid" => &[],
                "refused" => &[Connecting, Refused],
                _ => &[Connecting, Failed],
            },
        );
        config = configuration("ready");
    }
    let pid = useful(&config);
    let first = check(&receiver, &[Connecting, Connected]);
    assert_eq!(useful(&config), pid);
    check(&receiver, &[]);
    if mode == "replace" {
        config["private"]["env"]["PRIVATE_SECRET"] = json!("private-changed-environment");
        let next = useful(&config);
        assert_ne!(next, pid);
        reaped(pid);
        let events = drain(&receiver);
        assert_eq!(events.len(), 4);
        let expected = [CloseRequested, Disconnected, Connecting, Connected];
        for (event, outcome) in events.iter().zip(expected) {
            assert_eq!(event.fact, Fact::ToolServer { outcome });
        }
        assert_eq!(events[0].correlation, first[0].correlation);
        assert_eq!(events[0].correlation, events[1].correlation);
        assert_eq!(events[2].correlation, events[3].correlation);
        assert_ne!(events[0].correlation, events[2].correlation);
        mcp_client::shutdown();
        reaped(next);
        let ended = check(&receiver, &[CloseRequested, Disconnected]);
        assert_eq!(ended[0].correlation, events[2].correlation);
    } else {
        mcp_client::shutdown();
        reaped(pid);
        let ended = check(&receiver, &[CloseRequested, Disconnected]);
        assert_eq!(ended[0].correlation, first[0].correlation);
    }
    mcp_client::shutdown();
    check(&receiver, &[]);
    assert_eq!(loss(), Loss::default());
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--server") {
        fixture::serve(args.get(1).expect("server mode"));
        return;
    }
    if args.first().is_some_and(|arg| arg == "--probe") {
        probe(args.get(1).expect("probe mode"));
        return;
    }
    for mode in [
        "ready",
        "replace",
        "refused",
        "spawn",
        "discovery",
        "timeout",
        "off",
        "invalid",
        "disabled",
        "late",
        "full",
        "closed",
        "http",
        "http-auth",
        "http-oauth-missing",
        "http-discovery",
        "race-replace",
        "race-shutdown",
        "race-replace-disabled",
        "race-shutdown-disabled",
        "race-replace-full",
        "race-shutdown-full",
        "race-replace-closed",
        "race-shutdown-closed",
    ] {
        let root = Scratch::new("event-servers", mode);
        let work = root.join("work");
        std::fs::create_dir_all(&work).expect("workspace");
        let exe = std::env::current_exe().expect("candidate test executable");
        std::fs::copy(&exe, work.join("private-server")).expect("selected server artifact");
        let namespace = native_namespace::NativeNamespace::new(&root, &work);
        let mut command = namespace.command(exe);
        if mode.starts_with("http") {
            command.env("CASPER_JAIL", r#"{"reach":true}"#);
        }
        let mut child = command
            .args(["--probe", mode])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("isolated namespace");
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(status) = child.try_wait().expect("probe status") {
                assert!(status.success(), "scenario {mode}: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill timed out probe");
                child.wait().expect("reap probe");
                panic!("scenario {mode} exceeded deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("event servers scenario {mode}: passed");
    }
}
