use super::*;
use std::{
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
};

#[test]
fn fault_launcher() {
    let Ok(mode) = std::env::var("CASPER_TEST_AUTH_FAULT") else {
        return;
    };
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(std::env::var_os("CASPER_TEST_AUTH_STDOUT").expect("native stdout path"))
        .expect("stdout");
    rustix::stdio::dup2_stdout(&output).expect("native stdout descriptor");
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule,
    };
    let rules = match mode.as_str() {
        "file-sync" => [(libc::SYS_fsync, vec![])].into_iter().collect(),
        "rename" => [(libc::SYS_renameat, vec![]), (libc::SYS_renameat2, vec![])]
            .into_iter()
            .collect(),
        "stage-create" => {
            let flags = (libc::O_CREAT | libc::O_EXCL) as u64;
            let condition = SeccompCondition::new(
                2,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::MaskedEq(flags),
                flags,
            )
            .expect("flags");
            [(
                libc::SYS_openat,
                vec![SeccompRule::new(vec![condition]).expect("rule")],
            )]
            .into_iter()
            .collect()
        }
        _ => panic!("unknown fault"),
    };
    let program: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EIO as u32),
        std::env::consts::ARCH.try_into().expect("architecture"),
    )
    .expect("filter")
    .try_into()
    .expect("program");
    seccompiler::apply_filter(&program).expect("fault filter");
    let error = std::process::Command::new(binary())
        .args(["mcp-auth", "remote"])
        .env_remove("CASPER_TEST_AUTH_FAULT")
        .env_remove("CASPER_TEST_AUTH_STDOUT")
        .exec();
    panic!("exec native Casper: {error}");
}

fn prepared(server: &Server, root: &Scratch) -> (Store, PathBuf) {
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
    let path = root
        .join("state/casper/mcp-auth")
        .join(format!("{}.lock", binding.key()));
    let store = Store::at(&root.join("state/casper/mcp-auth"), binding).expect("store");
    store
        .replace(
            0,
            casper::mcp_client::oauth::Credential {
                access_token: "original-secret-canary".into(),
                refresh_token: Some("original-refresh-secret-canary".into()),
                expires_at: None,
                scopes: vec![],
            },
        )
        .expect("original credential");
    (store, path)
}

struct Login {
    child: tokio::process::Child,
    errors: BufReader<tokio::process::ChildStderr>,
    callback: url::Url,
}

impl Login {
    async fn start(server: &Server, mut command: tokio::process::Command) -> Self {
        let mut child = command.spawn().expect("login process");
        let mut errors = BufReader::new(child.stderr.take().expect("stderr"));
        let mut heading = String::new();
        errors.read_line(&mut heading).await.expect("heading");
        assert!(heading.contains("Open this URL"), "{heading}");
        let mut link = String::new();
        errors.read_line(&mut link).await.expect("link");
        let url = url::Url::parse(link.trim()).expect("authorization URL");
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        *server.challenge.lock().expect("challenge") = query["code_challenge"].clone();
        let mut callback = url::Url::parse(&query["redirect_uri"]).expect("callback");
        callback
            .query_pairs_mut()
            .append_pair("state", &query["state"])
            .append_pair("code", "code-secret-canary")
            .append_pair("iss", &server.origin);
        assert_image(&child);
        Self {
            child,
            errors,
            callback,
        }
    }

    async fn consent(&self) {
        let host = format!(
            "{}:{}",
            self.callback.host_str().expect("host"),
            self.callback.port().expect("port")
        );
        let mut socket = tokio::net::TcpStream::connect(&host)
            .await
            .expect("callback connection");
        socket
            .write_all(
                format!(
                    "GET {}?{} HTTP/1.1\r\nHost: {host}\r\n\r\n",
                    self.callback.path(),
                    self.callback.query().expect("query")
                )
                .as_bytes(),
            )
            .await
            .expect("callback request");
        let mut reply = String::new();
        socket
            .read_to_string(&mut reply)
            .await
            .expect("callback response");
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    }

    async fn finish(mut self, output_file: Option<&Path>, deadline: Duration) -> Value {
        let output = tokio::time::timeout(deadline, self.child.wait_with_output())
            .await
            .expect("native exit deadline")
            .expect("output");
        let mut errors = String::new();
        self.errors
            .read_to_string(&mut errors)
            .await
            .expect("stderr");
        assert!(!errors.contains("secret-canary"), "{errors}");
        let bytes = output_file.map_or(output.stdout, |path| {
            std::fs::read(path).expect("native stdout")
        });
        assert!(!String::from_utf8_lossy(&bytes).contains("secret-canary"));
        let reply = serde_json::from_slice(&bytes).expect("native family reply");
        let address = format!(
            "{}:{}",
            self.callback.host_str().expect("host"),
            self.callback.port().expect("port")
        );
        assert!(
            tokio::net::TcpStream::connect(address).await.is_err(),
            "callback listener leaked"
        );
        reply
    }
}

fn unchanged(store: &Store, root: &Scratch) {
    let state = store.snapshot().expect("state");
    assert_eq!(state.generation, 1);
    assert_eq!(state.epoch, 1);
    let credential = state.credential.expect("original");
    assert_eq!(credential.access_token, "original-secret-canary");
    assert_eq!(
        credential.refresh_token.as_deref(),
        Some("original-refresh-secret-canary")
    );
    for entry in std::fs::read_dir(root.join("state/casper/mcp-auth")).expect("store directory") {
        assert!(
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"),
            "temporary credential leaked"
        );
    }
}

async fn recovery(server: &Server, root: &Scratch, store: &Store) {
    let mut command = server.command(root, "mcp-auth");
    command.arg("remote");
    let login = Login::start(server, command).await;
    login.consent().await;
    let reply = login.finish(None, Duration::from_secs(3)).await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"][0]["authenticated"], true, "{reply}");
    let state = store.snapshot().expect("recovered");
    assert_eq!(state.generation, 2);
    assert_eq!(state.epoch, 2);
    assert_eq!(
        state.credential.expect("credential").access_token,
        "access-secret-canary"
    );
}

#[tokio::test]
async fn native_kernel_store_failures_are_visible_atomic_and_recoverable() {
    tokio::time::timeout(Duration::from_secs(25), async {
        for mode in ["stage-create", "file-sync", "rename"] {
            let server = Server::new().await;
            let root = Scratch::new("oauth", mode);
            let (store, _) = prepared(&server, &root);
            let stdout = root.join("native.stdout");
            let mut command = server.launch(
                &root,
                &std::env::current_exe().expect("launcher"),
                "--exact",
            );
            command
                .args(["commit::fault_launcher", "--nocapture"])
                .env("CASPER_TEST_AUTH_FAULT", mode)
                .env("CASPER_TEST_AUTH_STDOUT", &stdout);
            let login = Login::start(&server, command).await;
            login.consent().await;
            let reply = login.finish(Some(&stdout), Duration::from_secs(3)).await;
            assert_eq!(reply["ok"], false, "{mode}: {reply}");
            assert!(
                reply["error"]
                    .as_str()
                    .expect("error")
                    .contains("credential store"),
                "{mode}: {reply}"
            );
            assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
            unchanged(&store, &root);
            recovery(&server, &root, &store).await;
            assert_eq!(server.tokens.load(Ordering::SeqCst), 2);
        }
    })
    .await
    .expect("native fault scenarios deadline");
}

#[tokio::test]
async fn native_signals_fence_a_blocked_commit_without_persisting_or_late_resurrection() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for signal in [rustix::process::Signal::INT, rustix::process::Signal::TERM] {
            let server = Server::new().await;
            let root = Scratch::new("oauth", "commit-cancel");
            let (store, path) = prepared(&server, &root);
            let mut command = server.command(&root, "mcp-auth");
            command.arg("remote");
            let mut login = Login::start(&server, command).await;
            let lock = super::deadlines::locked(&path).await;
            login.consent().await;
            super::deadlines::waiting(&mut login.child, &path).await;
            assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
            let pid =
                rustix::process::Pid::from_raw(login.child.id().expect("pid") as i32).expect("PID");
            let descriptor = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
                .expect("pidfd");
            rustix::process::pidfd_send_signal(&descriptor, signal).expect("cancel signal");
            let reply = login.finish(None, Duration::from_millis(750)).await;
            assert_eq!(reply["ok"], false, "{reply}");
            assert!(
                reply["error"]
                    .as_str()
                    .expect("error")
                    .contains("cancelled"),
                "{reply}"
            );
            drop(lock);
            unchanged(&store, &root);
            recovery(&server, &root, &store).await;
            assert_eq!(server.tokens.load(Ordering::SeqCst), 2);
        }
    })
    .await
    .expect("native commit cancellation deadline");
}

#[tokio::test]
async fn native_login_deadline_fences_the_actual_post_exchange_commit() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let server = Server::new().await;
        let root = Scratch::new("oauth", "commit-deadline");
        let (store, path) = prepared(&server, &root);
        let mut auth = server.auth();
        auth.timeout_secs = 2;
        let mut command = server.command(&root, "mcp-auth");
        command.arg("remote").env(
            "CASPER_CONFIGURE",
            json!({"mcp_servers":{"remote":{"url":format!("{}/mcp",server.origin),"auth":auth}}})
                .to_string(),
        );
        let mut login = Login::start(&server, command).await;
        let lock = super::deadlines::locked(&path).await;
        login.consent().await;
        super::deadlines::waiting(&mut login.child, &path).await;
        assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
        let reply = login.finish(None, Duration::from_secs(3)).await;
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"]
                .as_str()
                .expect("error")
                .contains("timed out"),
            "{reply}"
        );
        drop(lock);
        unchanged(&store, &root);
        recovery(&server, &root, &store).await;
    })
    .await
    .expect("commit deadline scenario");
}
