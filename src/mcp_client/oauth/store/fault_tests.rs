use super::*;
use crate::{mcp_client::oauth::cancellation::Operation, scratch::Scratch};
use std::{os::fd::AsRawFd, sync::Arc};

fn store(root: &Scratch) -> Arc<Store> {
    let config: super::super::Config = serde_json::from_str(r#"{"issuer":"https://issuer.example","resource":"https://resource.example/mcp","client_id":"native"}"#).expect("config");
    Arc::new(
        Store::at(
            &root.join("store"),
            config.bind(&config.resource).expect("binding"),
        )
        .expect("store"),
    )
}

fn credential(value: &str) -> Credential {
    Credential {
        access_token: value.into(),
        refresh_token: None,
        expires_at: None,
        scopes: vec![],
    }
}

#[test]
fn kernel_write_and_sync_failures_never_report_persistence_success() {
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule,
    };
    for mode in ["write", "file-sync", "directory-sync"] {
        let root = Scratch::new("oauth", mode);
        let store = store(&root);
        store
            .replace(0, credential("original-secret-canary"))
            .expect("seed");
        let held = Arc::clone(&store);
        let result = std::thread::spawn(move || {
            let rules = if mode == "directory-sync" {
                vec![
                    SeccompRule::new(vec![
                        SeccompCondition::new(
                            0,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Eq,
                            held.directory.as_raw_fd() as u64,
                        )
                        .expect("condition"),
                    ])
                    .expect("rule"),
                ]
            } else {
                vec![]
            };
            let syscall = if mode == "write" {
                libc::SYS_write
            } else {
                libc::SYS_fsync
            };
            let filter = SeccompFilter::new(
                [(syscall, rules)].into_iter().collect(),
                SeccompAction::Allow,
                SeccompAction::Errno(libc::EIO as u32),
                std::env::consts::ARCH.try_into().expect("architecture"),
            )
            .expect("filter");
            let program: BpfProgram = filter.try_into().expect("program");
            seccompiler::apply_filter(&program).expect("thread-local fault");
            held.replace(1, credential("replacement-secret-canary"))
        })
        .join()
        .expect("fault thread");
        assert_eq!(result, Err(Error::Store), "{mode}");
        let state = store.snapshot().expect("read after failure");
        let (generation, token) = if mode == "directory-sync" {
            (2, "replacement-secret-canary")
        } else {
            (1, "original-secret-canary")
        };
        assert_eq!(state.generation, generation, "{mode}");
        assert_eq!(
            state.credential.expect("credential").access_token,
            token,
            "{mode}"
        );
        for entry in std::fs::read_dir(root.join("store")).expect("directory") {
            assert!(
                !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"),
                "uncommitted stage leaked"
            );
        }
        assert_eq!(
            store.replace(generation, credential("recovered")),
            Ok(generation + 1)
        );
    }
}

#[tokio::test]
async fn dropping_async_commit_fences_a_blocking_writer_and_releases_its_handles() {
    let root = Scratch::new("oauth", "cancel-commit");
    let store = store(&root);
    let lock = store.locked().expect("contended record lock");
    let held = Arc::clone(&store);
    let (started, ready) = tokio::sync::oneshot::channel();
    let (finished, result) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let operation = Operation::new(Duration::from_secs(5), None);
        let cancelled = Arc::clone(&operation.0);
        tokio::task::spawn_blocking(move || {
            started.send(()).expect("started");
            let result = held.replace_cancellable(0, credential("late-secret-canary"), &cancelled);
            drop(held);
            finished.send(result).expect("finished");
        })
        .await
        .expect("worker");
        drop(operation);
    });
    ready.await.expect("writer started");
    task.abort();
    assert!(task.await.expect_err("cancelled future").is_cancelled());
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(500), result)
            .await
            .expect("prompt cancelled lock wait")
            .expect("writer outcome"),
        Err(Error::Cancelled)
    );
    assert_eq!(Arc::strong_count(&store), 1);
    drop(lock);
    assert_eq!(store.snapshot().expect("unchanged").generation, 0);
    assert_eq!(store.replace(0, credential("successor")), Ok(1));
}

#[test]
fn expired_operation_cannot_commit_even_before_timeout_is_polled() {
    let root = Scratch::new("oauth", "commit-deadline");
    let store = store(&root);
    let operation = Operation::new(Duration::ZERO, None);
    assert_eq!(
        store.replace_cancellable(0, credential("late-secret-canary"), &operation.0),
        Err(Error::Timeout)
    );
    assert!(store.snapshot().expect("unchanged").credential.is_none());
}
