use super::*;
use std::time::Duration;

fn instance(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discarded_pending_and_completed_launches_reap_their_native_child() {
    for completed in [false, true] {
        let mut tasks = tokio::task::JoinSet::new();
        let (send, ready) = tokio::sync::oneshot::channel();
        let task = tasks.spawn(async move {
            let prepared = crate::jail::workflow_test_shell("exec sleep 30");
            let launched =
                spawn(prepared, Arc::new(AtomicBool::new(false))).expect("native launch");
            send.send(launched.server.1.id().expect("child PID"))
                .expect("started");
            if !completed {
                std::future::pending::<()>().await;
            }
            launched
        });
        let pid = tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .expect("launch bound")
            .expect("live child");
        let started = instance(pid).expect("native child instance");
        if completed {
            tokio::time::timeout(Duration::from_secs(1), async {
                while !task.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("completed task retains its output");
        } else {
            assert!(!task.is_finished());
        }
        assert_eq!(instance(pid).as_ref(), Some(&started));
        drop(tasks);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !task.is_finished() || instance(pid).as_ref() == Some(&started) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancelled launch reaped child");
    }
}

#[tokio::test]
async fn discarded_queued_launch_never_enters_the_spawn_body() {
    let entered = Arc::new(AtomicBool::new(false));
    let executing = entered.clone();
    let mut tasks = tokio::task::JoinSet::new();
    let task = tasks.spawn(async move {
        executing.store(true, Ordering::Release);
    });
    drop(tasks);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued launch cancelled");
    assert!(!entered.load(Ordering::Acquire));
}
