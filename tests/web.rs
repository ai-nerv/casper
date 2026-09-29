use casper::scratch::Scratch;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Site {
    url: String,
    calls: Arc<AtomicUsize>,
    ending: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let ending = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = ending.clone();
        let count = calls.clone();
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .expect("fixture read timeout");
                let mut buffer = [0; 8192];
                let n = stream.read(&mut buffer).expect("fixture request");
                count.fetch_add(1, Ordering::Relaxed);
                let request = String::from_utf8_lossy(&buffer[..n]);
                let target = request.split_whitespace().nth(1).unwrap_or("/");
                let (status, kind, headers, body) = if target.starts_with("/search?") {
                    (200, "application/json", "", json!({"results":[{"url":"https://example.com/result", "title":"Fixture search", "content":"A sourced result"}]}).to_string())
                } else if target == "/redirect" {
                    (302, "text/plain", "Location: /page\r\n", String::new())
                } else if target == "/unsafe" {
                    (
                        302,
                        "text/plain",
                        "Location: file:///etc/passwd\r\n",
                        String::new(),
                    )
                } else if target == "/loop" {
                    (302, "text/plain", "Location: /loop\r\n", String::new())
                } else if target == "/large" {
                    (200, "text/plain", "", "a".repeat(5000))
                } else if target == "/missing" {
                    (404, "text/plain", "", "not found".into())
                } else {
                    (
                        200,
                        "text/html; charset=utf-8",
                        "",
                        format!(
                            "<html><head><title>Fixture page</title></head><body><nav>Navigation noise</nav><main><h1>Verified heading</h1><p>{}</p><script>EXCLUDED_SECRET</script><a href='/next'>Next page</a><a href='javascript:alert(1)'>Unsafe</a></main></body></html>",
                            "世界 content ".repeat(40)
                        ),
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: {kind}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url,
            calls,
            ending,
            thread: Some(thread),
        }
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        self.ending.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fixture server joined");
        }
    }
}

fn call(args: Value, settings: Value, jail: Option<&str>) -> Value {
    let dir = Scratch::new("cw", "run");
    let config = dir.join("casper");
    std::fs::create_dir_all(&config).expect("fixture config");
    std::fs::write(
        config.join("tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("fixture declarations");
    let mut command = Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .arg("run")
        .current_dir(&dir)
        .env("HOME", &*dir)
        .env("XDG_CONFIG_HOME", &*dir)
        .env("XDG_RUNTIME_DIR", &*dir)
        .env("XDG_DATA_HOME", &*dir)
        .env("CASPER_CONFIGURE", json!({"web": settings}).to_string())
        .env_remove("CASPER_JAIL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(jail) = jail {
        command.env("CASPER_JAIL", jail);
    }
    let mut child = command.spawn().expect("Casper starts");
    child
        .stdin
        .take()
        .expect("fixture stdin")
        .write_all(json!({"tool":"web", "args": args}).to_string().as_bytes())
        .expect("tool call input");
    let output = child.wait_with_output().expect("Casper completes");
    assert!(output.status.success());
    let reply: Value = serde_json::from_slice(&output.stdout).expect("Casper JSON reply");
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"][0].clone()
}

#[test]
fn readable_pages_have_sources_links_unicode_paging_and_no_script_noise() {
    let site = Site::start();
    let ran = call(
        json!({"action":"fetch", "url":format!("{}/page", site.url), "max_chars":17}),
        json!({"allow_private":true}),
        None,
    );
    assert_ne!(ran["failed"], true, "{ran}");
    let page: Value =
        serde_json::from_str(ran["said"].as_str().expect("page text")).expect("page JSON");
    assert_eq!(page["title"], "Fixture page");
    assert_eq!(
        page["text"]
            .as_str()
            .expect("extracted text")
            .chars()
            .count(),
        17
    );
    assert_eq!(page["next_offset"], 17);
    assert_eq!(page["links"].as_array().expect("links").len(), 1);
    assert_eq!(page["links"][0]["url"], format!("{}/next", site.url));
    let rest = call(
        json!({"action":"fetch", "url":format!("{}/page", site.url), "offset":17}),
        json!({"allow_private":true}),
        None,
    );
    assert!(rest["said"].as_str().expect("page text").contains("世界"));
    assert!(
        !rest["said"]
            .as_str()
            .expect("page text")
            .contains("EXCLUDED_SECRET")
    );
    assert!(
        !rest["said"]
            .as_str()
            .expect("page text")
            .contains("Navigation noise")
    );
}

#[test]
fn private_access_cannot_be_granted_in_tool_arguments_and_the_jail_is_respected() {
    let site = Site::start();
    let url = format!("{}/page", site.url);
    let ran = call(json!({"action":"fetch", "url":url}), json!({}), None);
    assert_eq!(ran["failed"], true);
    assert_eq!(site.calls.load(Ordering::Relaxed), 0);
    let ran = call(
        json!({"action":"fetch", "url":url, "allow_private":true}),
        json!({}),
        None,
    );
    assert_eq!(ran["failed"], true);
    assert!(
        ran["said"]
            .as_str()
            .expect("error")
            .contains("unknown field")
    );
    let ran = call(
        json!({"action":"fetch", "url":url}),
        json!({"allow_private":true}),
        Some("1"),
    );
    assert_eq!(ran["failed"], true);
    assert!(ran["said"].as_str().expect("error").contains("reach grant"));
    assert_eq!(site.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn redirects_download_limits_and_http_failures_are_not_silent() {
    let site = Site::start();
    let ran = call(
        json!({"action":"fetch", "url":format!("{}/redirect", site.url)}),
        json!({"allow_private":true}),
        None,
    );
    assert_ne!(ran["failed"], true, "{ran}");
    let page: Value = serde_json::from_str(ran["said"].as_str().expect("page")).expect("JSON page");
    assert_eq!(page["url"], format!("{}/page", site.url));
    for (path, error) in [
        ("unsafe", "HTTP(S)"),
        ("loop", "five redirects"),
        ("large", "download limit"),
        ("missing", "HTTP 404"),
    ] {
        let ran = call(
            json!({"action":"fetch", "url":format!("{}/{path}", site.url)}),
            json!({"allow_private":true, "max_bytes":1024}),
            None,
        );
        assert_eq!(ran["failed"], true, "{ran}");
        assert!(
            ran["said"].as_str().expect("error").contains(error),
            "{ran}"
        );
    }
}

#[test]
fn configured_search_returns_sources_without_a_model_call() {
    let site = Site::start();
    let ran = call(
        json!({"action":"search", "query":"Rust & robots", "limit":2}),
        json!({"allow_private":true, "searxng":format!("{}/search", site.url)}),
        Some(r#"{"reach":true}"#),
    );
    assert_ne!(ran["failed"], true, "{ran}");
    let found: Value =
        serde_json::from_str(ran["said"].as_str().expect("search")).expect("search JSON");
    assert_eq!(found["provider"], "searxng");
    assert_eq!(found["query"], "Rust & robots");
    assert_eq!(found["results"][0]["url"], "https://example.com/result");
    assert_eq!(site.calls.load(Ordering::Relaxed), 1);
}
