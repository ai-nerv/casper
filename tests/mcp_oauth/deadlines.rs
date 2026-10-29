use super::*;

pub(super) async fn locked(path: &std::path::Path) -> std::fs::File {
    let lock = std::fs::File::open(path).expect("record lock");
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
    .expect("fixture lock acquisition");
    lock
}

fn prepared(server: &Server, root: &Scratch) -> (Store, std::path::PathBuf) {
    std::fs::create_dir_all(root.join("config/casper")).expect("config");
    std::fs::write(
        root.join("config/casper/tools.lua"),
        include_str!("../../config/tools.lua"),
    )
    .expect("tools");
    let binding = server
        .auth()
        .bind(&format!("{}/mcp", server.origin))
        .expect("binding");
    let lock = root
        .join("state/casper/mcp-auth")
        .join(format!("{}.lock", binding.key()));
    let store = Store::at(&root.join("state/casper/mcp-auth"), binding).expect("store");
    store.snapshot().expect("create lock");
    (store, lock)
}

pub(super) async fn waiting(child: &mut tokio::process::Child, path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            assert!(
                child.try_wait().expect("child state").is_none(),
                "child exited before lock wait"
            );
            let directory = format!("/proc/{}/fd", child.id().expect("pid"));
            if std::fs::read_dir(directory)
                .expect("child descriptors")
                .filter_map(Result::ok)
                .any(|entry| std::fs::read_link(entry.path()).is_ok_and(|target| target == path))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("lock wait observed");
}

#[tokio::test]
async fn native_login_deadline_includes_initial_record_lock_and_reaps_the_reader() {
    let server = Server::new().await;
    let root = Scratch::new("oauth", "initial-deadline");
    let (store, path) = prepared(&server, &root);
    let lock = locked(&path).await;
    let mut auth = server.auth();
    auth.timeout_secs = 1;
    let began = std::time::Instant::now();
    let mut child = server
        .command(&root, "mcp-auth")
        .arg("remote")
        .env(
            "CASPER_CONFIGURE",
            json!({"mcp_servers":{"remote":{"url":format!("{}/mcp",server.origin),"auth":auth}}})
                .to_string(),
        )
        .spawn()
        .expect("login");
    assert_image(&child);
    waiting(&mut child, &path).await;
    let output = tokio::time::timeout(Duration::from_millis(1500), child.wait_with_output())
        .await
        .expect("one-second operation and prompt reader teardown")
        .expect("output");
    assert!(began.elapsed() < Duration::from_millis(1750));
    let reply: Value = serde_json::from_slice(&output.stdout).expect("reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .expect("error")
            .contains("timed out"),
        "{reply}"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Open this URL"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    drop(lock);
    assert_eq!(store.snapshot().expect("unchanged").generation, 0);
}

#[tokio::test]
async fn native_login_signal_cancels_a_contended_initial_reader_before_consent() {
    let server = Server::new().await;
    let root = Scratch::new("oauth", "initial-cancel");
    let (store, path) = prepared(&server, &root);
    let lock = locked(&path).await;
    let mut child = server
        .command(&root, "mcp-auth")
        .arg("remote")
        .spawn()
        .expect("login");
    assert_image(&child);
    waiting(&mut child, &path).await;
    let process = rustix::process::Pid::from_raw(child.id().expect("pid") as i32).expect("PID");
    let descriptor =
        rustix::process::pidfd_open(process, rustix::process::PidfdFlags::empty()).expect("pidfd");
    rustix::process::pidfd_send_signal(&descriptor, rustix::process::Signal::TERM).expect("cancel");
    let output = tokio::time::timeout(Duration::from_millis(750), child.wait_with_output())
        .await
        .expect("prompt cancel and reader cleanup")
        .expect("output");
    let reply: Value = serde_json::from_slice(&output.stdout).expect("reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .expect("error")
            .contains("cancelled"),
        "{reply}"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Open this URL"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    drop(lock);
    assert_eq!(store.snapshot().expect("unchanged").generation, 0);
}
