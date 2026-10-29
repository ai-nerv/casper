use super::{configuration, drain, fixture, reaped, useful};
use casper::{events::*, mcp_client};
use serde_json::json;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn entered(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(value) = std::fs::read_to_string(".private-entered")
            && let Ok(namespace_pid) = value.parse()
        {
            assert_eq!(fixture::host_pid(namespace_pid), pid);
            break;
        }
        assert!(Instant::now() < deadline, "native request did not enter");
        thread::sleep(Duration::from_millis(5));
    }
}

fn identities(events: &[Event]) {
    use ServerOutcome::*;
    assert_eq!(events.len(), 8);
    let mut ids = Vec::new();
    for event in events {
        let id = event
            .correlation
            .request_id
            .expect("native session identity");
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    assert_eq!(ids.len(), 2, "old and recovery sessions only");
    for id in ids {
        let session: Vec<_> = events
            .iter()
            .filter(|event| event.correlation.request_id == Some(id))
            .collect();
        assert!(
            session
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        let outcomes: Vec<_> = events
            .iter()
            .filter(|event| event.correlation.request_id == Some(id))
            .map(|event| match event.fact {
                Fact::ToolServer { outcome } => outcome,
                _ => panic!("server event"),
            })
            .collect();
        assert_eq!(
            outcomes,
            [Connecting, Connected, CloseRequested, Disconnected]
        );
    }
}

pub fn probe(mode: &str) {
    let disabled = mode.ends_with("-disabled");
    let full = mode.ends_with("-full");
    let closed = mode.ends_with("-closed");
    let receiver = if disabled {
        None
    } else {
        Some(enable_local().expect("explicit local receiver"))
    };
    let receiver = if closed {
        drop(receiver);
        None
    } else {
        receiver
    };
    if full {
        for _ in 0..QUEUED_EVENTS {
            assert_eq!(
                emit(|| (
                    Correlation::default(),
                    Fact::ToolServer {
                        outcome: ServerOutcome::Unknown
                    }
                )),
                Emitted::Queued
            );
        }
    }
    let mut config = configuration("ready");
    config["private"]["timeout_secs"] = json!(20);
    let old_pid = useful(&config);
    let native = config.clone();
    let (finished, result) = mpsc::sync_channel(1);
    let caller = thread::spawn(move || {
        finished
            .send(mcp_client::call(
                Some(&native),
                "mcp__private__echo",
                &json!({"text":"private-hold"}),
            ))
            .expect("return native call");
    });
    entered(old_pid);
    assert!(
        matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "call must still own the session"
    );
    let began = Instant::now();
    let new_pid = if mode.starts_with("race-replace") {
        let next = std::env::current_dir()
            .expect("workspace")
            .join("private-replacement-server");
        std::fs::copy(
            config["private"]["command"].as_str().expect("old artifact"),
            &next,
        )
        .expect("selected replacement artifact");
        config["private"]["command"] = json!(next);
        config["private"]["env"]["PRIVATE_SECRET"] = json!("private-replacement");
        useful(&config)
    } else {
        mcp_client::shutdown();
        reaped(old_pid);
        useful(&config)
    };
    let ran = result
        .recv_timeout(Duration::from_secs(3))
        .expect("native cancellation must settle before request deadline");
    caller.join().expect("native caller joined");
    assert!(
        ran.failed,
        "cancelled transport cannot report useful success"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "close must not wait for20second call timeout"
    );
    assert_ne!(new_pid, old_pid);
    reaped(old_pid);
    assert_eq!(useful(&config), new_pid, "recovery session reused");
    assert_eq!(
        std::fs::read_to_string(".private-executions").expect("execution evidence"),
        "hold\n",
        "native cancelled request must never be retried"
    );
    mcp_client::shutdown();
    reaped(new_pid);
    mcp_client::shutdown();
    if disabled {
        let receiver = enable_local().expect("activation after native work");
        assert!(drain(&receiver).is_empty());
        assert_eq!(loss(), Loss::default());
    } else if closed {
        assert_eq!(
            loss(),
            Loss {
                closed: 8,
                ..Loss::default()
            }
        );
    } else {
        let receiver = receiver.expect("live receiver");
        if full {
            for _ in 0..QUEUED_EVENTS {
                receiver.try_recv().expect("original queue");
            }
            assert!(receiver.try_recv().is_err());
            assert_eq!(
                loss(),
                Loss {
                    full: 8,
                    ..Loss::default()
                }
            );
        } else {
            identities(&drain(&receiver));
            assert_eq!(loss(), Loss::default());
        }
    }
}
