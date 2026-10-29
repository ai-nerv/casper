#![allow(clippy::expect_used)]
use super::*;
use std::sync::Arc;

fn outcomes(receiver: &events::Receiver) -> Vec<ServerOutcome> {
    let mut result = Vec::new();
    let mut id = None;
    while let Ok(bytes) = receiver.try_recv() {
        let event = events::decode(&bytes).expect("strict event");
        let actual = event.correlation.request_id.expect("attempt identity");
        assert_eq!(*id.get_or_insert(actual), actual);
        assert_eq!(
            event.correlation,
            Correlation {
                request_id: Some(actual),
                ..Correlation::default()
            }
        );
        let Fact::ToolServer { outcome } = event.fact else {
            panic!("server event")
        };
        result.push(outcome);
    }
    result
}

#[cfg(test)]
#[test]
fn isolated_lifecycle() {
    use std::process::Command;
    let Ok(mode) = std::env::var("CASPER_SERVER_TRACE_PROBE") else {
        for mode in [
            "disabled", "default", "dropped", "unwind", "failed", "refused", "ready", "shared",
        ] {
            let output = Command::new(std::env::current_exe().expect("unit executable"))
                .args([
                    "--exact",
                    "mcp_client::observing::tests::isolated_lifecycle",
                    "--nocapture",
                ])
                .env("CASPER_SERVER_TRACE_PROBE", mode)
                .output()
                .expect("isolated probe");
            assert!(
                output.status.success(),
                "trace scenario {mode}: {}\n{}",
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
    if mode == "disabled" {
        let mut trace = Trace::new();
        let receiver = events::enable_local().expect("late activation");
        trace.connected();
        trace.closing();
        drop(trace);
        assert!(outcomes(&receiver).is_empty());
        assert_eq!(events::loss(), events::Loss::default());
        return;
    }
    let receiver = events::enable_local().expect("activation");
    use ServerOutcome::*;
    let expected = match mode.as_str() {
        "default" => {
            drop(Trace::default());
            vec![]
        }
        "dropped" => {
            drop(Trace::new());
            vec![Connecting, Unknown]
        }
        "unwind" => {
            assert!(
                std::panic::catch_unwind(|| {
                    let _trace = Trace::new();
                    panic!("private-error");
                })
                .is_err()
            );
            vec![Connecting, Unknown]
        }
        "failed" | "refused" => {
            let mut trace = Trace::new();
            let outcome = if mode == "failed" { Failed } else { Refused };
            trace.finish(outcome);
            trace.finish(Failed);
            trace.connected();
            trace.closing();
            drop(trace);
            vec![Connecting, outcome]
        }
        "ready" => {
            let mut trace = Trace::new();
            trace.connected();
            trace.connected();
            drop(trace);
            vec![Connecting, Connected, Disconnected]
        }
        "shared" => {
            let mut trace = Trace::new();
            trace.connected();
            let trace = Arc::new(trace);
            std::thread::scope(|scope| {
                for _ in 0..16 {
                    let trace = trace.clone();
                    scope.spawn(move || trace.closing());
                }
            });
            let remaining = trace.clone();
            drop(trace);
            assert_eq!(outcomes(&receiver), [Connecting, Connected, CloseRequested]);
            drop(remaining);
            vec![Disconnected]
        }
        _ => panic!("unknown scenario"),
    };
    assert_eq!(outcomes(&receiver), expected);
    assert_eq!(events::loss(), events::Loss::default());
}
