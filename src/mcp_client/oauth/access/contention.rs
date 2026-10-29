use super::*;
use crate::{mcp_client::oauth::Credential, scratch::Scratch};
use std::path::Path;

fn fixture(root: &Path) -> (Config, Arc<Store>, std::path::PathBuf) {
    let config: Config = serde_json::from_str(r#"{"issuer":"https://issuer.example","resource":"https://resource.example/mcp","client_id":"contended-reader","timeout_secs":1}"#).expect("config");
    let binding = config.bind(&config.resource).expect("binding");
    let path = root.join(format!("{}.lock", binding.key()));
    let store = Arc::new(Store::at(root, binding).expect("store"));
    store
        .replace(
            0,
            Credential {
                access_token: "access-secret-canary".into(),
                refresh_token: None,
                expires_at: None,
                scopes: vec![],
            },
        )
        .expect("seed");
    (config, store, path)
}

async fn locked(path: &Path) -> std::fs::File {
    let lock = std::fs::File::open(path).expect("lock");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("fixture lock: {error}"),
            }
        }
    })
    .await
    .expect("fixture ready");
    lock
}

fn descriptors(path: &Path) -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("descriptors")
        .filter_map(Result::ok)
        .filter(|entry| std::fs::read_link(entry.path()).is_ok_and(|target| target == path))
        .count()
}

async fn reading(path: &Path) {
    tokio::time::timeout(Duration::from_millis(1750), async {
        while descriptors(path) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("contended reader observed");
}

#[tokio::test]
async fn initial_attachment_abort_and_deadline_release_contended_readers() {
    for abort in [true, false] {
        let root = Scratch::new("oauth", "attach-reader");
        let (config, store, path) = fixture(&root.join("store"));
        let lock = locked(&path).await;
        let held = store.clone();
        let started = std::time::Instant::now();
        let task =
            tokio::spawn(async move { Access::attach(&config, &config.resource, held).await });
        reading(&path).await;
        if abort {
            task.abort();
        }
        let result =
            tokio::time::timeout(Duration::from_millis(if abort { 500 } else { 1250 }), task)
                .await
                .expect("attachment terminates");
        if abort {
            assert!(result.err().expect("aborted task").is_cancelled());
        } else {
            assert!(matches!(result.expect("task"), Err(Error::Timeout)));
            assert!(started.elapsed() < Duration::from_millis(1750));
        }
        tokio::time::timeout(Duration::from_millis(500), async {
            while Arc::strong_count(&store) != 1 || descriptors(&path) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader released before fixture unlock");
        drop(lock);
        assert_eq!(store.snapshot().expect("unchanged").generation, 1);
    }
}

#[tokio::test]
async fn stopped_and_discarded_monitors_release_contended_readers() {
    for stop in [false, true] {
        let root = Scratch::new("oauth", "monitor-reader");
        let (config, store, path) = fixture(&root.join("store"));
        let access = Access::attach(&config, &config.resource, store.clone())
            .await
            .expect("access");
        let monitor = access
            .monitor
            .lock()
            .expect("monitor")
            .as_ref()
            .expect("task")
            .abort_handle();
        let lock = locked(&path).await;
        reading(&path).await;
        let weak = Arc::downgrade(&access);
        if stop {
            access.stop();
            tokio::time::timeout(Duration::from_millis(500), async {
                while !monitor.is_finished()
                    || Arc::strong_count(&store) != 2
                    || descriptors(&path) != 1
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("stopped monitor releases reader");
        }
        drop(access);
        tokio::time::timeout(Duration::from_millis(500), async {
            while weak.upgrade().is_some()
                || !monitor.is_finished()
                || Arc::strong_count(&store) != 1
                || descriptors(&path) != 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("discarded monitor releases reader");
        drop(lock);
        assert_eq!(store.snapshot().expect("unchanged").generation, 1);
    }
}
