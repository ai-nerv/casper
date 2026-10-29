use super::*;
use crate::{mcp_client::oauth::Credential, scratch::Scratch};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn cancelled_refresh_releases_its_contended_reader_before_the_record_lock() {
    for abort in [false, true] {
        let server = Server::new("rotated").await;
        let root = Scratch::new("oauth", "cancel-reader");
        let store = server.store(&root);
        let config = server.config();
        let endpoint = format!("{}/mcp", server.origin);
        let path = root.join("store").join(format!(
            "{}.lock",
            config.bind(&endpoint).expect("binding").key()
        ));
        let lock = std::fs::File::open(&path).expect("record lock");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                {
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
        let cancelled = Arc::new(AtomicBool::new(false));
        let (held, stopping) = (store.clone(), cancelled.clone());
        let task =
            tokio::spawn(async move { current(&held, &config, &endpoint, 1, stopping).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let readers = std::fs::read_dir("/proc/self/fd")
                    .expect("descriptors")
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        std::fs::read_link(entry.path()).is_ok_and(|target| target == path)
                    })
                    .count();
                if readers == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocked reader observed");
        if abort {
            task.abort();
        } else {
            cancelled.store(true, Ordering::Release);
        }
        let result = tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("prompt cancellation");
        if abort {
            assert!(result.expect_err("aborted task").is_cancelled());
        } else {
            assert!(matches!(result.expect("task"), Err(Error::Cancelled)));
        }
        tokio::time::timeout(Duration::from_millis(500), async {
            while Arc::strong_count(&store) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking reader released ownership");
        assert_eq!(server.tokens.load(Ordering::SeqCst), 0);
        drop(lock);
        assert_eq!(store.snapshot().expect("unchanged").generation, 1);
    }
}

struct Server {
    origin: String,
    tokens: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn new(mode: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let tokens = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (base, count, permit) = (origin.clone(), tokens.clone(), release.clone());
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.expect("accept");
                let mut reader = BufReader::new(socket);
                let mut first = String::new();
                reader.read_line(&mut first).await.expect("line");
                let path = first.split_whitespace().nth(1).expect("path");
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.expect("header");
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().expect("length");
                    }
                }
                assert!(length < 65536);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.expect("body");
                let mut headers = String::new();
                let (status, body) = match path {
                    "/mcp" => {
                        headers = format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/metadata\"\r\n");
                        (401,String::new())
                    },
                    "/metadata" => (200,json!({"resource":format!("{base}/mcp"),"authorization_servers":[base]}).to_string()),
                    "/.well-known/oauth-authorization-server" => (200,json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"]}).to_string()),
                    "/token" => {
                        let form: std::collections::BTreeMap<_,_> = url::form_urlencoded::parse(&body).into_owned().collect();
                        assert_eq!(form.get("grant_type").map(String::as_str),Some("refresh_token"));
                        assert_eq!(form.get("refresh_token").map(String::as_str),Some("refresh-secret-canary"));
                        assert_eq!(form.get("resource").map(String::as_str),Some(format!("{base}/mcp").as_str()));
                        assert_eq!(form.get("client_id").map(String::as_str),Some("native-client"));
                        count.fetch_add(1,Ordering::SeqCst);
                        if mode == "paused" { permit.acquire().await.expect("release").forget(); }
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        if mode == "malformed" { (200, "\"refresh-secret-canary\"".into()) }
                        else if mode == "huge" { (200, "refresh-secret-canary".repeat(4096)) }
                        else if mode == "failed" { (400,json!({"error":"invalid_grant","error_description":"refresh-secret-canary"}).to_string()) }
                        else { (200,json!({"access_token":"new-access-secret-canary","refresh_token":"rotated-secret-canary","token_type":"Bearer","expires_in":3600}).to_string()) }
                    },
                    _ => (404,String::new()),
                };
                let mut socket = reader.into_inner();
                let _ = socket.write_all(format!("HTTP/1.1 {status} Test\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            }
        });
        Self {
            origin,
            tokens,
            release,
            task,
        }
    }

    fn config(&self) -> Config {
        serde_json::from_value(json!({"issuer":self.origin,"resource":format!("{}/mcp",self.origin),"client_id":"native-client","private_origins":[self.origin],"timeout_secs":3})).expect("config")
    }

    fn store(&self, root: &Scratch) -> Arc<Store> {
        let store = Arc::new(
            Store::at(
                &root.join("store"),
                self.config()
                    .bind(&format!("{}/mcp", self.origin))
                    .expect("binding"),
            )
            .expect("store"),
        );
        store
            .replace(
                0,
                Credential {
                    access_token: "old-access-secret-canary".into(),
                    refresh_token: Some("refresh-secret-canary".into()),
                    expires_at: Some(1),
                    scopes: vec!["read".into()],
                },
            )
            .expect("seed");
        store
    }

    async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.tokens.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("refresh start");
    }
}

#[tokio::test]
async fn invalid_and_huge_token_responses_preserve_credentials_and_release_refresh_lease() {
    for mode in ["malformed", "huge"] {
        let server = Server::new(mode).await;
        let root = Scratch::new("oauth", "bad-token");
        let store = server.store(&root);
        let error = current(
            &store,
            &server.config(),
            &format!("{}/mcp", server.origin),
            1,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect_err("rejected token");
        assert!(matches!(error, Error::Credential | Error::Metadata));
        assert!(!error.to_string().contains("secret-canary"));
        let state = store.snapshot().expect("snapshot");
        let credential = state.credential.expect("old credential");
        assert_eq!(credential.access_token, "old-access-secret-canary");
        assert_eq!(
            credential.refresh_token.as_deref(),
            Some("refresh-secret-canary")
        );
        assert_eq!(state.epoch, 1);
        assert_eq!(state.generation, 2);
        assert!(store.try_refresh().expect("lock").is_some());
        assert!(matches!(
            current(
                &store,
                &server.config(),
                &format!("{}/mcp", server.origin),
                1,
                Arc::new(AtomicBool::new(false))
            )
            .await,
            Err(Error::Backoff)
        ));
        assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_expired_callers_share_one_rotation_and_preserve_epoch() {
    let server = Server::new("rotated").await;
    let root = Scratch::new("oauth", "refresh-shared");
    let store = server.store(&root);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let config = server.config();
        let endpoint = format!("{}/mcp", server.origin);
        let store = Arc::new(
            Store::at(
                &root.join("store"),
                config.bind(&endpoint).expect("binding"),
            )
            .expect("independent store handle"),
        );
        tasks.push(tokio::spawn(async move {
            current(
                &store,
                &config,
                &endpoint,
                1,
                Arc::new(AtomicBool::new(false)),
            )
            .await
        }));
    }
    for task in tasks {
        let state = task.await.expect("task").expect("refresh");
        assert_eq!(state.epoch, 1);
        assert_eq!(state.generation, 3);
        let credential = state.credential.expect("credential");
        assert_eq!(
            credential.refresh_token.as_deref(),
            Some("rotated-secret-canary")
        );
        assert_eq!(credential.scopes, ["read"]);
    }
    assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
    assert!(store.snapshot().expect("state").refresh_after.is_none());
}

#[tokio::test]
async fn logout_during_refresh_does_not_wait_for_network_or_resurrect_credentials() {
    let server = Server::new("paused").await;
    let root = Scratch::new("oauth", "refresh-logout");
    let store = server.store(&root);
    let (held, config, endpoint) = (
        store.clone(),
        server.config(),
        format!("{}/mcp", server.origin),
    );
    let task = tokio::spawn(async move {
        current(
            &held,
            &config,
            &endpoint,
            1,
            Arc::new(AtomicBool::new(false)),
        )
        .await
    });
    server.started().await;
    let held = store.clone();
    let generation = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking(move || held.logout()),
    )
    .await
    .expect("logout did not wait for network")
    .expect("thread")
    .expect("logout");
    server.release.add_permits(1);
    assert_eq!(
        task.await.expect("task").expect_err("late response"),
        Error::Changed
    );
    let state = store.snapshot().expect("state");
    assert_eq!(state.generation, generation);
    assert_eq!(state.epoch, generation);
    assert!(state.credential.is_none());
}

#[tokio::test]
async fn failed_refresh_is_sanitized_preserves_material_and_backs_off_other_callers() {
    let server = Server::new("failed").await;
    let root = Scratch::new("oauth", "refresh-failed");
    let store = server.store(&root);
    let config = server.config();
    let endpoint = format!("{}/mcp", server.origin);
    let error = current(
        &store,
        &config,
        &endpoint,
        1,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect_err("failure");
    assert_eq!(error, Error::Required);
    assert!(!error.to_string().contains("secret-canary"));
    for _ in 0..4 {
        assert_eq!(
            current(
                &store,
                &config,
                &endpoint,
                1,
                Arc::new(AtomicBool::new(false))
            )
            .await
            .expect_err("backoff"),
            Error::Backoff
        );
    }
    assert_eq!(server.tokens.load(Ordering::SeqCst), 1);
    let state = store.snapshot().expect("state");
    assert_eq!(state.epoch, 1);
    assert_eq!(
        state.credential.expect("retained").refresh_token.as_deref(),
        Some("refresh-secret-canary")
    );
}

#[tokio::test]
async fn cancellation_releases_refresh_lock_and_refuses_late_commit() {
    let server = Server::new("paused").await;
    let root = Scratch::new("oauth", "refresh-cancel");
    let store = server.store(&root);
    let cancelled = Arc::new(AtomicBool::new(false));
    let (held, config, endpoint, flag) = (
        store.clone(),
        server.config(),
        format!("{}/mcp", server.origin),
        cancelled.clone(),
    );
    let task = tokio::spawn(async move { current(&held, &config, &endpoint, 1, flag).await });
    server.started().await;
    cancelled.store(true, Ordering::Release);
    server.release.add_permits(1);
    assert_eq!(
        task.await.expect("task").expect_err("cancelled"),
        Error::Cancelled
    );
    assert!(store.try_refresh().expect("lock").is_some());
    assert_eq!(
        store
            .snapshot()
            .expect("state")
            .credential
            .expect("old")
            .access_token,
        "old-access-secret-canary"
    );
}
