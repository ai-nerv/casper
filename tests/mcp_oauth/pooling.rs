use super::*;
use casper::mcp_client::{self, oauth::Credential};
use std::io::{BufRead, Write};

#[derive(Default)]
pub(super) struct Pause {
    stage: AtomicUsize,
    entered: AtomicUsize,
    closed: AtomicUsize,
}

impl Pause {
    pub(super) async fn wait(
        &self,
        stage: usize,
        reader: &mut BufReader<tokio::net::TcpStream>,
    ) -> bool {
        if self.stage.load(Ordering::SeqCst) != stage {
            return true;
        }
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut byte = [0];
        assert_eq!(
            reader
                .read(&mut byte)
                .await
                .expect("peer closes cancelled request"),
            0
        );
        self.closed.fetch_add(1, Ordering::SeqCst);
        false
    }
}

fn useful(config: &Value, name: &str) {
    let reply = mcp_client::call(Some(config), name, &json!({}));
    assert!(!reply.failed, "{}", reply.said);
    assert!(reply.said.contains("authenticated tool result"));
}

fn signal(text: &str) {
    println!("POOL:{text}");
    std::io::stdout().flush().expect("flush");
}

#[test]
fn pool_worker() {
    let Ok(mode) = std::env::var("CASPER_TEST_POOL_MODE") else {
        return;
    };
    let config: Value =
        serde_json::from_str(&std::env::var("CASPER_TEST_POOL_CONFIG").expect("config"))
            .expect("JSON");
    useful(&config, "mcp__other__proof");
    let selected = config.clone();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let caller = std::thread::spawn(move || {
        send.send(mcp_client::call(
            Some(&selected),
            "mcp__remote__proof",
            &json!({}),
        ))
        .expect("result");
    });
    let input = std::io::stdin();
    let mut lines = input.lock().lines();
    assert_eq!(lines.next().expect("action").expect("line"), "cancel");
    let began = std::time::Instant::now();
    if mode.starts_with("shutdown") {
        mcp_client::shutdown();
    } else if mode.starts_with("logout") {
        let result =
            mcp_client::authentication::command("mcp-logout", &["remote".into()], Some(&config))
                .expect("logout");
        assert_eq!(result["authenticated"], false);
    }
    let reply = receive
        .recv_timeout(Duration::from_millis(if mode.starts_with("external") {
            1750
        } else {
            750
        }))
        .expect("in-flight request cancelled without its network deadline");
    assert!(reply.failed, "cancelled call cannot succeed");
    assert!(!reply.said.contains("secret-canary"));
    caller.join().expect("caller joined");
    assert!(
        began.elapsed()
            < Duration::from_millis(if mode.starts_with("external") {
                1900
            } else {
                1000
            })
    );
    useful(&config, "mcp__other__proof");
    signal("cancelled");
    assert_eq!(lines.next().expect("recovery").expect("line"), "recover");
    useful(&config, "mcp__remote__proof");
    mcp_client::shutdown();
    mcp_client::shutdown();
    let state = std::path::PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("state"));
    assert!(
        !std::fs::read_dir("/proc/self/fd")
            .expect("descriptors")
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .any(|path| path.starts_with(&state)),
        "credential descriptors retained after shutdown"
    );
    signal("finished");
}

fn credential(expired: bool) -> Credential {
    Credential {
        access_token: "access-secret-canary".into(),
        refresh_token: Some("refresh-secret-canary".into()),
        expires_at: if expired { Some(1) } else { None },
        scopes: vec!["read".into()],
    }
}

async fn count(counter: &AtomicUsize, expected: usize, deadline: Duration) {
    tokio::time::timeout(deadline, async {
        while counter.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("observed server barrier");
    assert_eq!(counter.load(Ordering::SeqCst), expected);
}

async fn marker(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    expected: &str,
) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("output")
                .expect("worker alive");
            if line == format!("POOL:{expected}") {
                break;
            }
        }
    })
    .await
    .expect("worker stage");
}

#[tokio::test]
async fn pooled_shutdown_and_local_external_logout_cancel_owned_work_without_replay() {
    for (mode, stage) in [
        ("shutdown-refresh", 1),
        ("logout-refresh", 1),
        ("external-refresh", 1),
        ("shutdown-call", 2),
        ("logout-call", 2),
        ("external-call", 2),
        ("shutdown-initialize", 3),
        ("shutdown-discovery", 4),
        ("shutdown-record", 5),
    ] {
        println!("pool scenario: {mode}");
        let server = Server::new().await;
        let other = Server::new().await;
        server.pause.stage.store(stage, Ordering::SeqCst);
        let root = Scratch::new("oauth", mode);
        std::fs::create_dir_all(root.join("config/casper")).expect("config");
        std::fs::write(
            root.join("config/casper/tools.lua"),
            include_str!("../../config/tools.lua"),
        )
        .expect("native command registry");
        let store = Store::at(
            &root.join("state/casper/mcp-auth"),
            server
                .auth()
                .bind(&format!("{}/mcp", server.origin))
                .expect("binding"),
        )
        .expect("store");
        store
            .replace(0, credential(stage == 1))
            .expect("seed remote");
        let unrelated = Store::at(
            &root.join("state/casper/mcp-auth"),
            other
                .auth()
                .bind(&format!("{}/mcp", other.origin))
                .expect("other binding"),
        )
        .expect("other store");
        unrelated.replace(0, credential(false)).expect("seed other");
        let path = root.join("state/casper/mcp-auth").join(format!(
            "{}.lock",
            server
                .auth()
                .bind(&format!("{}/mcp", server.origin))
                .expect("binding")
                .key()
        ));
        let lock = if stage == 5 {
            Some(super::deadlines::locked(&path).await)
        } else {
            None
        };
        let config = json!({"remote":{"url":format!("{}/mcp",server.origin),"auth":server.auth(),"timeout_secs":20},
            "other":{"url":format!("{}/mcp",other.origin),"auth":other.auth(),"timeout_secs":20}});
        let executable = std::env::current_exe().expect("test owner image");
        let mut child = server
            .launch(&root, &executable, "--exact")
            .args(["pooling::pool_worker", "--nocapture"])
            .env("CASPER_TEST_POOL_MODE", mode)
            .env("CASPER_TEST_POOL_CONFIG", config.to_string())
            .stdin(Stdio::piped())
            .spawn()
            .expect("isolated owner probe");
        let mut input = child.stdin.take().expect("control");
        let mut output = BufReader::new(child.stdout.take().expect("output")).lines();
        if stage == 5 {
            super::deadlines::waiting(&mut child, &path).await;
        } else {
            tokio::select! {
                _ = count(&server.pause.entered, 1, Duration::from_secs(3)) => {},
                status = child.wait() => {
                    let mut diagnostic = String::new();
                    child.stderr.take().expect("stderr").read_to_string(&mut diagnostic).await.expect("diagnostic");
                    panic!("{mode} worker exited before barrier: {status:?}: {diagnostic}");
                }
            }
        }
        assert_eq!(
            std::fs::read_link(format!("/proc/{}/exe", child.id().expect("pid")))
                .expect("actual owner image"),
            executable.canonicalize().expect("image")
        );
        if mode.starts_with("external") {
            let result = server.invoke(&root, "mcp-logout").await;
            assert_eq!(result["result"][0]["authenticated"], false, "{result}");
        }
        input.write_all(b"cancel\n").await.expect("cancel");
        marker(&mut output, "cancelled").await;
        if stage == 5 {
            assert!(
                !std::fs::read_dir(format!("/proc/{}/fd", child.id().expect("pid")))
                    .expect("descriptors")
                    .filter_map(Result::ok)
                    .any(
                        |entry| std::fs::read_link(entry.path()).is_ok_and(|target| target == path)
                    ),
                "initial reader closed before fixture unlock"
            );
            assert_eq!(server.requests.load(Ordering::SeqCst), 0);
        } else {
            count(&server.pause.closed, 1, Duration::from_millis(500)).await;
        }
        drop(lock);
        assert_eq!(server.calls.load(Ordering::SeqCst), usize::from(stage == 2));
        assert_eq!(
            server.tokens.load(Ordering::SeqCst),
            usize::from(stage == 1)
        );
        assert_eq!(
            other.calls.load(Ordering::SeqCst),
            2,
            "other server survives cancellation"
        );
        assert_eq!(
            other.initialized.load(Ordering::SeqCst),
            if mode.starts_with("shutdown") { 2 } else { 1 },
            "logout must preserve the unrelated pooled session"
        );
        let state = store.snapshot().expect("state");
        if !mode.starts_with("shutdown") {
            assert!(
                state.credential.is_none(),
                "logout cannot resurrect credentials"
            );
        } else {
            assert_eq!(state.epoch, 1);
            assert_eq!(
                state.credential.as_ref().expect("preserved").access_token,
                "access-secret-canary"
            );
        }
        server.pause.stage.store(0, Ordering::SeqCst);
        store
            .replace(state.generation, credential(false))
            .expect("explicit successor login");
        input.write_all(b"recover\n").await.expect("recover");
        marker(&mut output, "finished").await;
        let result = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .expect("probe exit")
            .expect("output");
        assert!(
            result.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&result.stderr).contains("secret-canary"));
        assert_eq!(
            server.calls.load(Ordering::SeqCst),
            usize::from(stage == 2) + 1,
            "no replay before recovery"
        );
        assert_eq!(
            server.tokens.load(Ordering::SeqCst),
            usize::from(stage == 1),
            "no replayed refresh"
        );
        assert_eq!(unrelated.snapshot().expect("other state").generation, 1);
    }
}
