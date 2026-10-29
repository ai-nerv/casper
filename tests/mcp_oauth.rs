#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::{
    mcp_client::oauth::{Config, Store},
    scratch::Scratch,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[path = "mcp_oauth/deadlines.rs"]
mod deadlines;

#[path = "mcp_oauth/commit.rs"]
mod commit;

#[path = "mcp_oauth/pooling.rs"]
mod pooling;

fn binary() -> std::path::PathBuf {
    let selected = std::env::var_os("CASPER_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_casper").into());
    std::fs::canonicalize(selected).expect("selected Casper executable")
}

fn assert_image(child: &tokio::process::Child) {
    assert_eq!(
        std::fs::canonicalize(format!("/proc/{}/exe", child.id().expect("pid")))
            .expect("running image"),
        binary()
    );
}

struct Server {
    origin: String,
    requests: Arc<AtomicUsize>,
    tokens: Arc<AtomicUsize>,
    challenge: Arc<Mutex<String>>,
    calls: Arc<AtomicUsize>,
    initialized: Arc<AtomicUsize>,
    lose_reply: Arc<AtomicBool>,
    pause: Arc<pooling::Pause>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("server");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let (requests, tokens, challenge) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(String::new())),
        );
        let (base, count, issued, expected) = (
            origin.clone(),
            requests.clone(),
            tokens.clone(),
            challenge.clone(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::new(AtomicUsize::new(0));
        let initialization = initialized.clone();
        let lose_reply = Arc::new(AtomicBool::new(false));
        let (called, lost) = (calls.clone(), lose_reply.clone());
        let pause = Arc::new(pooling::Pause::default());
        let held = pause.clone();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    Some(result) = children.join_next(), if !children.is_empty() => { result.expect("HTTP task"); },
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("accept");
                        let (base, count, issued, expected) = (base.clone(), count.clone(), issued.clone(), expected.clone());
                        let (called, lost) = (called.clone(), lost.clone());
                        let initialization = initialization.clone();
                        let held = held.clone();
                        children.spawn(async move {
                            tokio::time::timeout(Duration::from_secs(5), async move {
                                let mut reader = BufReader::new(socket);
                                let mut header = String::new();
                                loop {
                                    let mut line = String::new();
                                    reader.read_line(&mut line).await.expect("header");
                                    if line == "\r\n" || line.is_empty() { break; }
                                    header.push_str(&line);
                                    assert!(header.len() <= 8192);
                                }
                                count.fetch_add(1, Ordering::SeqCst);
                                let mut first = header.split_whitespace();
                                let method = first.next().expect("method");
                                let path = first.next().expect("path");
                                let headers: BTreeMap<_,_> = header.lines().skip(1).filter_map(|line| line.split_once(':').map(|(name, value)| (name.to_ascii_lowercase(),value.trim().to_owned()))).collect();
                                let length: usize = headers.get("content-length").map(|value| value.parse().expect("length")).unwrap_or(0);
                                assert!(length <= 65536);
                                let mut body = vec![0; length];
                                reader.read_exact(&mut body).await.expect("body");
                                let mut extra = String::new();
                                let (status, output) = match (method, path) {
                                    ("GET", "/mcp") if !headers.contains_key("authorization") => {
                                        extra = format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/metadata\"\r\n");
                                        (401, String::new())
                                    },
                                    ("GET", "/metadata") => (200, json!({"resource":format!("{base}/mcp"),"authorization_servers":[base],"scopes_supported":["read"]}).to_string()),
                                    ("GET", "/.well-known/oauth-authorization-server") => (200, json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"],"authorization_response_iss_parameter_supported":true}).to_string()),
                                    ("POST", "/token") => {
                                        use base64::Engine;
                                        use sha2::{Digest, Sha256};
                                        assert!(!headers.contains_key("authorization"));
                                        let form: BTreeMap<_,_> = url::form_urlencoded::parse(&body).into_owned().collect();
                                        if form["grant_type"] == "refresh_token" {
                                            assert_eq!(form["refresh_token"],"refresh-secret-canary");
                                            assert!(!form.contains_key("code"));
                                        } else {
                                            assert_eq!(form["grant_type"], "authorization_code");
                                            assert_eq!(form["code"], "code-secret-canary");
                                            assert_eq!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())), *expected.lock().expect("proof"));
                                        }
                                        assert_eq!(form["resource"], format!("{base}/mcp"));
                                        assert_eq!(form["client_id"], "registered-native");
                                        issued.fetch_add(1, Ordering::SeqCst);
                                        if !held.wait(1, &mut reader).await { return; }
                                        let refresh = if form["grant_type"] == "refresh_token" {"rotated-secret-canary"} else {"refresh-secret-canary"};
                                        (200, json!({"access_token":"access-secret-canary","refresh_token":refresh,"token_type":"Bearer","expires_in":3600,"scope":"read"}).to_string())
                                    },
                                    ("POST", "/mcp") => {
                                        assert_eq!(headers.get("authorization").map(String::as_str), Some("Bearer access-secret-canary"));
                                        let message: Value = serde_json::from_slice(&body).expect("MCP");
                                        let stage = match message["method"].as_str() {
                                            Some("initialize") => { initialization.fetch_add(1, Ordering::SeqCst); 3 },
                                            Some("tools/list") => 4,
                                            _ => 0,
                                        };
                                        if stage != 0 && !held.wait(stage, &mut reader).await { return; }
                                        let result = match message["method"].as_str().expect("method") {
                                            "initialize" => json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"oauth-fixture","version":"1"}}),
                                            "tools/list" => json!({"tools":[{"name":"proof","description":"auth proof","inputSchema":{"type":"object"}}]}),
                                            "resources/list" => json!({"resources":[]}),
                                            "prompts/list" => json!({"prompts":[]}),
                                            "tools/call" => {
                                                called.fetch_add(1, Ordering::SeqCst);
                                                if !held.wait(2, &mut reader).await { return; }
                                                if lost.load(Ordering::SeqCst) { return; }
                                                json!({"content":[{"type":"text","text":"authenticated tool result"}]})
                                            },
                                            _ => json!({}),
                                        };
                                        if message.get("id").is_none() { (202, String::new()) }
                                        else { (200, json!({"jsonrpc":"2.0","id":message["id"],"result":result}).to_string()) }
                                    },
                                    _ => (405, String::new()),
                                };
                                let mut socket = reader.into_inner();
                                socket.write_all(format!("HTTP/1.1 {status} Test\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{output}",output.len()).as_bytes()).await.expect("reply");
                            }).await.expect("request deadline");
                        });
                    },
                }
            }
        });
        Self {
            origin,
            requests,
            tokens,
            challenge,
            calls,
            initialized,
            lose_reply,
            pause,
            task,
        }
    }

    fn auth(&self) -> Config {
        serde_json::from_value(json!({"issuer":self.origin,"resource":format!("{}/mcp",self.origin),"client_id":"registered-native","private_origins":[self.origin],"timeout_secs":5})).expect("config")
    }

    fn command(&self, root: &Scratch, verb: &str) -> tokio::process::Command {
        self.launch(root, &binary(), verb)
    }

    fn launch(
        &self,
        root: &Scratch,
        binary: &std::path::Path,
        verb: &str,
    ) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(binary);
        command.arg(verb).current_dir(&**root).env("HOME", &**root).env("XDG_CONFIG_HOME",root.join("config"))
            .env("XDG_DATA_HOME",root.join("data")).env("XDG_STATE_HOME",root.join("state"))
            .env("XDG_RUNTIME_DIR", &**root).env_remove("CASPER_JAIL")
            .env("CASPER_CONFIGURE",json!({"mcp_servers":{"remote":{"url":format!("{}/mcp",self.origin),"auth":self.auth()}}}).to_string())
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        command
    }

    async fn invoke(&self, root: &Scratch, verb: &str) -> Value {
        let mut command = self.command(root, verb);
        if verb.starts_with("mcp-") {
            command.arg("remote");
        }
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .expect("deadline")
            .expect("output");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("secret-canary"));
        }
        serde_json::from_slice(&output.stdout).expect("reply")
    }
}

#[tokio::test]
async fn native_tool_response_loss_reports_unknown_effect_without_replay_or_refresh() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut server = Server::new().await;
        server.lose_reply.store(true, Ordering::SeqCst);
        let root = Scratch::new("oauth", "native-no-replay");
        std::fs::create_dir_all(root.join("config/casper")).expect("config");
        std::fs::write(
            root.join("config/casper/tools.lua"),
            include_str!("../config/tools.lua"),
        )
        .expect("tools");
        let store = Store::at(
            &root.join("state/casper/mcp-auth"),
            server
                .auth()
                .bind(&format!("{}/mcp", server.origin))
                .expect("binding"),
        )
        .expect("store");
        store
            .replace(
                0,
                casper::mcp_client::oauth::Credential {
                    access_token: "access-secret-canary".into(),
                    refresh_token: Some("refresh-secret-canary".into()),
                    expires_at: None,
                    scopes: vec![],
                },
            )
            .expect("credential");
        let mut child = server
            .command(&root, "run")
            .stdin(Stdio::piped())
            .spawn()
            .expect("native process");
        assert_image(&child);
        let mut input = child.stdin.take().expect("stdin");
        input
            .write_all(br#"{"tool":"mcp__remote__proof","args":{}}"#)
            .await
            .expect("call");
        drop(input);
        let output = child.wait_with_output().await.expect("output");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reply: Value = serde_json::from_slice(&output.stdout).expect("reply");
        let ran: casper::tools::Ran =
            serde_json::from_value(reply["result"][0].clone()).expect("tool result");
        assert!(ran.failed, "{}", ran.said);
        assert!(ran.said.contains("not retried"), "{}", ran.said);
        assert!(
            ran.said
                .contains("remote effects may already have occurred"),
            "{}",
            ran.said
        );
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.tokens.load(Ordering::SeqCst), 0);
        assert_eq!(store.snapshot().expect("snapshot").generation, 1);
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("secret-canary"));
        }
        server.task.abort();
        assert!(
            (&mut server.task)
                .await
                .as_ref()
                .is_err_and(|error| error.is_cancelled())
        );
    })
    .await
    .expect("native no-replay deadline");
}

#[tokio::test]
async fn explicit_native_login_persists_private_credentials_and_logout_fences_discovery() {
    tokio::time::timeout(Duration::from_secs(25), async {
        let server = Server::new().await;
        let root = Scratch::new("oauth", "native");
        std::fs::create_dir_all(root.join("config/casper")).expect("config");
        std::fs::write(
            root.join("config/casper/tools.lua"),
            include_str!("../config/tools.lua"),
        )
        .expect("tools");
        let before = server.invoke(&root, "mcp-status").await;
        assert_eq!(before["result"][0]["authenticated"], false);
        let _ = server.invoke(&root, "tools").await;
        assert_eq!(
            server.requests.load(Ordering::SeqCst),
            0,
            "background discovery must not start interactive auth"
        );
        let mut child = server
            .command(&root, "mcp-auth")
            .arg("remote")
            .spawn()
            .expect("auth");
        assert_image(&child);
        let mut errors = BufReader::new(child.stderr.take().expect("stderr"));
        let mut heading = String::new();
        errors.read_line(&mut heading).await.expect("heading");
        assert!(heading.contains("Open this URL"), "{heading}");
        let mut link = String::new();
        errors.read_line(&mut link).await.expect("link");
        let url = url::Url::parse(link.trim()).expect("authorization link");
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        *server.challenge.lock().expect("challenge") = query["code_challenge"].clone();
        assert_eq!(query["code_challenge_method"], "S256");
        let mut callback = url::Url::parse(&query["redirect_uri"]).expect("callback");
        callback
            .query_pairs_mut()
            .append_pair("state", &query["state"])
            .append_pair("code", "code-secret-canary")
            .append_pair("iss", &server.origin);
        let host = format!(
            "{}:{}",
            callback.host_str().expect("host"),
            callback.port().expect("port")
        );
        let mut socket = tokio::net::TcpStream::connect(&host)
            .await
            .expect("callback connect");
        socket
            .write_all(
                format!(
                    "GET {}?{} HTTP/1.1\r\nHost: {host}\r\n\r\n",
                    callback.path(),
                    callback.query().expect("query")
                )
                .as_bytes(),
            )
            .await
            .expect("callback write");
        let mut received = String::new();
        socket
            .read_to_string(&mut received)
            .await
            .expect("callback reply");
        assert!(received.starts_with("HTTP/1.1 200"));
        let output = child.wait_with_output().await.expect("auth output");
        let mut remaining = String::new();
        errors.read_to_string(&mut remaining).await.expect("errors");
        assert!(output.status.success());
        assert!(!remaining.contains("secret-canary"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-canary"));
        let logged_in: Value = serde_json::from_slice(&output.stdout).expect("login reply");
        assert_eq!(logged_in["result"][0]["authenticated"], true, "{logged_in}");
        assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
        let status = server.invoke(&root, "mcp-status").await;
        assert_eq!(status["result"][0]["refresh_available"], true);
        let tools = server.invoke(&root, "tools").await;
        assert!(tools.to_string().contains("mcp__remote__proof"), "{tools}");
        let store = Store::at(
            &root.join("state/casper/mcp-auth"),
            server
                .auth()
                .bind(&format!("{}/mcp", server.origin))
                .expect("binding"),
        )
        .expect("store");
        let generation = store.snapshot().expect("snapshot").generation;
        let logout = server.invoke(&root, "mcp-logout").await;
        assert_eq!(logout["result"][0]["authenticated"], false);
        assert_eq!(
            store.snapshot().expect("logged out").generation,
            generation + 1
        );
        assert!(store.snapshot().expect("tombstone").credential.is_none());
        let count = server.requests.load(Ordering::SeqCst);
        let _ = server.invoke(&root, "tools").await;
        assert_eq!(count, server.requests.load(Ordering::SeqCst));
    })
    .await
    .expect("whole native scenario deadline");
}

#[tokio::test]
async fn missing_network_grant_refuses_before_consent_or_store_creation() {
    let server = Server::new().await;
    let root = Scratch::new("oauth", "no-grant");
    std::fs::create_dir_all(root.join("config/casper")).expect("config");
    std::fs::write(
        root.join("config/casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("tools");
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        server
            .command(&root, "mcp-auth")
            .arg("remote")
            .env("CASPER_JAIL", "1")
            .output(),
    )
    .await
    .expect("deadline")
    .expect("output");
    let reply: Value = serde_json::from_slice(&output.stdout).expect("reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        reply["error"].as_str().expect("error").contains("network"),
        "{reply}"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Open this URL"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    assert!(!root.join("state/casper/mcp-auth").exists());
}

#[tokio::test]
async fn independent_native_processes_refresh_once_before_authenticated_discovery() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = Server::new().await;
        let root = Scratch::new("oauth", "native-refresh");
        std::fs::create_dir_all(root.join("config/casper")).expect("config");
        std::fs::write(
            root.join("config/casper/tools.lua"),
            include_str!("../config/tools.lua"),
        )
        .expect("tools");
        let store = Store::at(
            &root.join("state/casper/mcp-auth"),
            server
                .auth()
                .bind(&format!("{}/mcp", server.origin))
                .expect("binding"),
        )
        .expect("store");
        store
            .replace(
                0,
                casper::mcp_client::oauth::Credential {
                    access_token: "expired-secret-canary".into(),
                    refresh_token: Some("refresh-secret-canary".into()),
                    expires_at: Some(1),
                    scopes: vec!["read".into()],
                },
            )
            .expect("expired");
        let mut children = Vec::new();
        for _ in 0..4 {
            let child = server
                .command(&root, "tools")
                .spawn()
                .expect("native process");
            assert_image(&child);
            children.push(child);
        }
        for child in children {
            let output = child.wait_with_output().await.expect("output");
            assert!(output.status.success());
            let reply: Value = serde_json::from_slice(&output.stdout).expect("reply");
            assert!(
                reply.to_string().contains("mcp__remote__proof"),
                "{reply} stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            for bytes in [&output.stdout, &output.stderr] {
                assert!(!String::from_utf8_lossy(bytes).contains("secret-canary"));
            }
        }
        assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
        let state = store.snapshot().expect("snapshot");
        assert_eq!(state.epoch, 1);
        assert_eq!(state.generation, 3);
        assert_eq!(
            state.credential.expect("rotated").refresh_token.as_deref(),
            Some("rotated-secret-canary")
        );
    })
    .await
    .expect("native refresh deadline");
}
