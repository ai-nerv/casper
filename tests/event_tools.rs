#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::{
    events::*,
    scratch::Scratch,
    tools::{Call, Ran},
    wire::Reply,
};
use serde_json::json;
use std::{
    collections::HashSet,
    process::Stdio,
    time::{Duration, Instant},
};

#[path = "support/native_namespace.rs"]
mod native_namespace;
use native_namespace::NativeNamespace;

const TOOLS: &str = r#"
casper.tool("private_success", {description="private-description", parameters={}, run=function() return {said="private-output", mutation={id="forged",outcome="committed"}} end})
casper.tool("private_failure", {description="failure", parameters={}, run=function() error("private-error") end})
casper.tool("private_nil", {description="nil", parameters={}, run=function() casper.exec("sh", {"-c", "printf changed > private-nil-effect"}); return nil end})
casper.tool("private_wait", {description="wait", parameters={}, run=function() return {shown={shown="wonder",wonder="helper",args={},about="private-question"}} end})
casper.tool("private_ask", {description="ask", parameters={}, run=function() return {shown={shown="ask",question="private-question",options={{id="private-answer",label="private-label"}}}} end})
casper.file_tool("private_read", {description="read",parameters={},operation="read"})
casper.file_tool("private_write", {description="write",parameters={},operation="write"})
casper.file_tool("private_bad_render", {description="write",parameters={},operation="write",render=function() error("private-render-error") end})
"#;

fn request(tool: &str) -> Call {
    serde_json::from_value(json!({"tool":tool,"args":{"private-argument":"private-value"}}))
        .expect("call")
}

fn write(tool: &str, expected: &str) -> Call {
    serde_json::from_value(
        json!({"tool":tool,"args":{"path":"private-source","contents":"private-content"},
        "file_revision":{"version":1,"operation":"write"},"revision_contract":1,
        "expected_revision":expected,"mutation_id":"private-mutation-id"}),
    )
    .expect("write")
}

fn drain(receiver: &Receiver) -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(bytes) = receiver.try_recv() {
        assert!(bytes.len() <= MAX_EVENT_BYTES);
        let text = std::str::from_utf8(&bytes).expect("JSON");
        for secret in ["private-", "private_", "forged", "CASPER", "HOME"] {
            assert!(!text.contains(secret), "event leaked {secret}");
        }
        events.push(decode(&bytes).expect("strict event"));
    }
    events
}

fn outcomes(events: &[Event]) -> Vec<ToolOutcome> {
    events
        .iter()
        .filter_map(|event| match event.fact {
            Fact::Tool { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect()
}

fn check_call(
    receiver: &Receiver,
    call: Call,
    expected: &[ToolOutcome],
) -> (Result<Ran, String>, Vec<Event>) {
    let result = casper::calling::ran(&call, None);
    let events = drain(receiver);
    assert_eq!(outcomes(&events), expected, "{result:?}");
    let id = events.first().expect("events").correlation.tool_call_id;
    assert!(id.is_some());
    assert!(
        events
            .iter()
            .all(|event| event.correlation.tool_call_id == id)
    );
    for event in &events {
        if let Fact::Tool {
            outcome,
            duration_ms,
        } = event.fact
        {
            assert_eq!(duration_ms.is_some(), outcome != ToolOutcome::Started);
        }
        let mut correlation = event.correlation;
        correlation.tool_call_id = None;
        assert_eq!(correlation, Correlation::default());
    }
    (result, events)
}

fn broker(valid: bool) -> Result<Ran, String> {
    let body = json!({"call":{"tool":"private_read","args":{"path":"private-source"}},
        "data":{"kind":"text","data":{"source_text":null,"text":"private-content","source_bytes":15,
        "total_lines":1,"first_line":if valid {1} else {0},"last_line":1,"omitted_bytes":0}}});
    casper::mcp::render_input(body.to_string().as_bytes())
}

fn probe(mode: &str) {
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    if mode == "disabled" {
        assert!(
            !casper::calling::ran(&request("private_success"), None)
                .expect("success")
                .failed
        );
        assert!(casper::calling::ran(&request("missing"), None).is_err());
        assert!(
            !casper::calling::ran(&write("private_write", "absent"), None)
                .expect("mutation")
                .failed
        );
        let receiver = enable_local().expect("activate after useful work");
        assert!(drain(&receiver).is_empty());
        assert_eq!(loss(), Loss::default());
        return;
    }
    let receiver = enable_local().expect("isolated explicit activation");
    if mode == "closed" {
        drop(receiver);
        assert!(
            !casper::calling::ran(&write("private_write", "absent"), None)
                .expect("mutation")
                .failed
        );
        assert_eq!(
            std::fs::read("private-source").expect("committed"),
            b"private-content"
        );
        assert_eq!(loss().closed, 3);
        return;
    }
    if mode == "full" {
        for _ in 0..QUEUED_EVENTS {
            assert_eq!(
                emit(|| (
                    Correlation::default(),
                    Fact::Tool {
                        outcome: ToolOutcome::Admitted,
                        duration_ms: None
                    }
                )),
                Emitted::Queued
            );
        }
        assert!(
            !casper::calling::ran(&write("private_write", "absent"), None)
                .expect("mutation")
                .failed
        );
        assert_eq!(
            std::fs::read("private-source").expect("committed"),
            b"private-content"
        );
        assert_eq!(loss().full, 3);
        assert_eq!(drain(&receiver).len(), QUEUED_EVENTS);
        let (result, _) = check_call(
            &receiver,
            request("private_success"),
            &[ToolOutcome::Started, ToolOutcome::Succeeded],
        );
        assert!(!result.expect("success").failed);
        return;
    }
    if mode == "broker" {
        assert!(!broker(true).expect("brokered presentation").failed);
        assert!(!std::path::Path::new("private-source").exists());
        assert_eq!(
            outcomes(&drain(&receiver)),
            [ToolOutcome::Started, ToolOutcome::Succeeded]
        );
        assert!(broker(false).is_err());
        assert_eq!(outcomes(&drain(&receiver)), [ToolOutcome::Refused]);
        return;
    }
    if mode == "ordinary" {
        let mut ids = HashSet::new();
        for (tool, expected) in [
            (
                "private_success",
                vec![ToolOutcome::Started, ToolOutcome::Succeeded],
            ),
            (
                "private_failure",
                vec![ToolOutcome::Started, ToolOutcome::Failed],
            ),
            (
                "private_nil",
                vec![ToolOutcome::Started, ToolOutcome::Unknown],
            ),
            (
                "private_wait",
                vec![ToolOutcome::Started, ToolOutcome::Waiting],
            ),
            (
                "private_ask",
                vec![ToolOutcome::Started, ToolOutcome::Waiting],
            ),
            ("missing", vec![ToolOutcome::Refused]),
        ] {
            let (_, events) = check_call(&receiver, request(tool), &expected);
            assert!(ids.insert(String::from(
                events[0].correlation.tool_call_id.expect("local id")
            )));
            assert!(
                events
                    .iter()
                    .all(|event| matches!(event.fact, Fact::Tool { .. }))
            );
        }
        assert_eq!(
            std::fs::read("private-nil-effect").expect("effect preceding missing result"),
            b"changed"
        );
        let guarded: Call = serde_json::from_value(
            json!({"tool":"private_success","file_revision":{"version":1,"operation":"read"}}),
        )
        .expect("guarded");
        assert!(
            check_call(&receiver, guarded, &[ToolOutcome::Refused])
                .0
                .is_err()
        );
        let settings = json!({"tools":{"private_success":{"off":true}}});
        assert!(
            casper::setup::apply(settings.as_object().expect("settings"))
                .refused
                .is_empty()
        );
        assert!(
            check_call(
                &receiver,
                request("private_success"),
                &[ToolOutcome::Refused]
            )
            .0
            .is_err()
        );
        return;
    }
    let tool = if mode == "render" {
        "private_bad_render"
    } else {
        "private_write"
    };
    if mode == "unknown" {
        std::fs::write(".drop-reply", b"owned fixture").expect("reply fault");
    }
    if mode == "stale" {
        std::fs::write("private-source", b"untouched").expect("existing source");
    }
    let terminal = match mode {
        "unknown" => ToolOutcome::Unknown,
        "stale" => ToolOutcome::Refused,
        "render" => ToolOutcome::Failed,
        "native" => ToolOutcome::Succeeded,
        _ => panic!("unexpected scenario {mode}"),
    };
    let (result, events) = check_call(
        &receiver,
        write(tool, "absent"),
        &[ToolOutcome::Started, terminal],
    );
    let result = result.expect("native result");
    assert_eq!(events.len(), 3);
    let mutation = &events[1].fact;
    match mode {
        "unknown" => {
            assert_eq!(
                *mutation,
                Fact::Mutation {
                    state: MutationState::Unknown {}
                }
            );
            assert_eq!(result.files.mutation.expect("receipt").outcome, "unknown");
        }
        "stale" => assert_eq!(
            *mutation,
            Fact::Mutation {
                state: MutationState::Refused {}
            }
        ),
        _ => assert_eq!(
            *mutation,
            Fact::Mutation {
                state: MutationState::Committed {
                    durability: Durability::Confirmed,
                    observation: Observation::Confirmed,
                    delivery: Delivery::Unconfirmed,
                }
            }
        ),
    }
    assert_eq!(
        std::fs::read("private-source").expect("disk evidence"),
        if mode == "stale" {
            b"untouched".as_slice()
        } else {
            b"private-content".as_slice()
        }
    );
    if mode == "native" {
        let call: Call = serde_json::from_value(json!({"tool":"private_read","args":{"path":"private-source"},"file_revision":{"version":1,"operation":"read"}})).expect("read");
        let (read, events) = check_call(
            &receiver,
            call,
            &[ToolOutcome::Started, ToolOutcome::Succeeded],
        );
        assert_eq!(events.len(), 2);
        assert_eq!(read.expect("read").files.observations.len(), 1);
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "code") {
        assert_eq!(args.get(1).map(String::as_str), Some("--file-worker"));
        let reply = match casper::files::worker::serve() {
            Ok(value) => Reply::of(value),
            Err(why) => Reply::refused(why.to_string()),
        };
        if std::path::Path::new(".drop-reply").exists() {
            print!("{{");
        } else {
            serde_json::to_writer(std::io::stdout().lock(), &reply).expect("worker reply");
        }
        return;
    }
    if args.first().is_some_and(|arg| arg == "--probe") {
        probe(args.get(1).expect("scenario"));
        return;
    }
    for mode in [
        "ordinary", "native", "stale", "render", "unknown", "broker", "disabled", "full", "closed",
    ] {
        let root = Scratch::new("event-tools", mode);
        std::fs::create_dir_all(root.join("work")).expect("workspace");
        let namespace = NativeNamespace::new(&root, &root.join("work"));
        std::fs::create_dir_all(root.join("config/casper")).expect("configuration");
        std::fs::write(root.join("config/casper/tools.lua"), TOOLS).expect("owned declarations");
        let mut child = namespace
            .command(std::env::current_exe().expect("test executable"))
            .args(["--probe", mode])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("isolated worker");
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(status) = child.try_wait().expect("worker status") {
                assert!(status.success(), "scenario {mode}: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill timed out worker");
                child.wait().expect("reap worker");
                panic!("scenario {mode} exceeded deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("event tools scenario {mode}: passed");
    }
}
