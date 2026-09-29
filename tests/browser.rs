use base64::Engine;
use casper::scratch::Scratch;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

struct Site {
    url: String,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener.set_nonblocking(true).expect("fixture listener");
        let url = format!("http://{}", listener.local_addr().expect("fixture address"));
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("fixture timeout");
                let mut request = [0; 8192];
                let Ok(count) = stream.read(&mut request) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&request[..count]);
                let next = request
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(|path| path.starts_with("/next"));
                let body = if next {
                    "<html><title>Second page</title><body><h1>Navigation succeeded</h1></body></html>"
                } else {
                    r#"<html><title>Casper browser fixture</title><body style="background:#18323d;color:#dff5ef;font:24px sans-serif;padding:40px">
<h1>Casper native browser</h1><p id="status">Ready for interaction</p>
<button id="button" onclick="document.querySelector('#status').innerText='Clicked successfully'">Click me</button>
<input id="name" aria-label="Robot name" oninput="document.querySelector('#echo').innerText=this.value">
<input id="secret" type="password" value="DO_NOT_EXPOSE_THIS"><p id="echo"></p>
<a id="next" href="/next">Second page</a><div style="height:1800px">Scrollable content</div></body></html>"#
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url,
            stopping,
            thread: Some(thread),
        }
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fixture closes");
        }
    }
}

fn fixture() -> Scratch {
    let dir = Scratch::in_dir(std::path::Path::new("/tmp"), "cb", "a");
    std::fs::create_dir_all(dir.join("casper")).expect("fixture config");
    std::fs::create_dir_all(dir.join("tmp")).expect("fixture tmp");
    std::fs::write(
        dir.join("casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("fixture declarations");
    dir
}

fn command(dir: &Scratch) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .current_dir(dir)
        .env("HOME", &**dir)
        .env("XDG_CONFIG_HOME", &**dir)
        .env("XDG_RUNTIME_DIR", &**dir)
        .env("XDG_DATA_HOME", &**dir)
        .env("TMPDIR", dir.join("tmp"))
        .env("CASPER_CONFIGURE", "{}")
        .env_remove("CASPER_JAIL")
        .env_remove("CASPER_BROWSER_ENDPOINT");
    command
}

fn call(dir: &Scratch, endpoint: &str, args: Value) -> Value {
    let mut child = command(dir)
        .arg("run")
        .env("CASPER_JAIL", r#"{"reach":true}"#)
        .env("CASPER_BROWSER_ENDPOINT", endpoint)
        .env("CASPER_CONFIGURE", r#"{"browser":{"allow_private":true}}"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Casper call");
    child
        .stdin
        .take()
        .expect("call input")
        .write_all(json!({"tool":"browse","args":args}).to_string().as_bytes())
        .expect("call input");
    let output = child.wait_with_output().expect("Casper finishes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Value = serde_json::from_slice(&output.stdout).expect("family reply");
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"][0].clone()
}

fn text(ran: &Value) -> Value {
    assert_ne!(ran["failed"], true, "{ran}");
    serde_json::from_str(ran["said"].as_str().expect("tool text")).expect("browser JSON")
}

fn program() -> String {
    std::env::var("CASPER_TEST_BROWSER").expect("run oslo make test-browser")
}
fn no_sandbox() -> bool {
    std::env::var("CASPER_TEST_NO_SANDBOX").as_deref() == Ok("1")
}

fn active_browser_processes(profile: &std::path::Path) -> Vec<u32> {
    let profile = profile.to_string_lossy();
    std::fs::read_dir("/proc")
        .expect("process directory")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let command = std::fs::read(entry.path().join("cmdline")).ok()?;
            let command = String::from_utf8_lossy(&command);
            let state = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            (command.contains("--user-data-dir=")
                && command.contains(profile.as_ref())
                && state.split_whitespace().nth(2) != Some("Z"))
            .then_some(pid)
        })
        .collect()
}

#[test]
fn browse_configuration_and_permissions_cannot_be_changed_by_tool_arguments() {
    let dir = fixture();
    for (configured, args, expected) in [
        (
            json!({"browser":{"endpoint":"http://example.com:9222"}}),
            json!({"action":"tabs"}),
            "literal loopback",
        ),
        (
            json!({"browser":{"endpoint":"http://127.0.0.1:9222"}}),
            json!({"action":"tabs","endpoint":"http://evil.test"}),
            "unknown field",
        ),
        (
            json!({"browser":{"endpoint":"http://user:pass@127.0.0.1:9222"}}),
            json!({"action":"tabs"}),
            "credential-free",
        ),
        (
            json!({"browser":{"endpoint":"http://127.0.0.1:9222"}}),
            json!({"action":"open","url":"http://127.0.0.1:12345"}),
            "private",
        ),
        (
            json!({"browser":{"endpoint":"http://127.0.0.1:9222"}}),
            json!({"action":"open","url":"file:///etc/passwd"}),
            "HTTP(S)",
        ),
    ] {
        let mut child = command(&dir)
            .arg("run")
            .env("CASPER_CONFIGURE", configured.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Casper call");
        child
            .stdin
            .take()
            .expect("call input")
            .write_all(json!({"tool":"browse","args":args}).to_string().as_bytes())
            .expect("call input");
        let reply: Value =
            serde_json::from_slice(&child.wait_with_output().expect("Casper finishes").stdout)
                .expect("reply");
        assert_eq!(reply["result"][0]["failed"], true, "{reply}");
        assert!(reply.to_string().contains(expected), "{reply}");
    }
    let mut child = command(&dir)
        .arg("run")
        .env("CASPER_JAIL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Casper call");
    child
        .stdin
        .take()
        .expect("call input")
        .write_all(br#"{"tool":"browse","args":{"action":"tabs"}}"#)
        .expect("call input");
    let output = child.wait_with_output().expect("Casper finishes");
    assert!(String::from_utf8_lossy(&output.stdout).contains("reach grant"));
    let output = command(&dir)
        .args(["browser", "--program", "/nonexistent/chromium"])
        .env("CASPER_JAIL", "1")
        .output()
        .expect("browser launch refused");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("inside CASPER_JAIL"));
}

#[test]
#[ignore = "requires the pinned Chromium runtime; run oslo make test-browser"]
fn real_chromium_navigation_dom_input_screenshot_and_profile_cleanup() {
    let dir = fixture();
    let site = Site::new();
    let browser =
        casper::browser::Managed::launch(&program(), no_sandbox()).expect("real Chromium starts");
    let pid = browser.pid();
    let profile = browser.profile().to_path_buf();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&profile)
            .expect("profile permissions")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let opened = text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"open","url":site.url}),
    ));
    assert_eq!(opened["title"], "Casper browser fixture", "{opened}");
    assert!(!opened.to_string().contains("DO_NOT_EXPOSE_THIS"));
    let target = opened["target"].as_str().expect("target");
    let clicked = text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"click","target":target,"selector":"#button"}),
    ));
    assert!(
        clicked["text"]
            .as_str()
            .expect("DOM text")
            .contains("Clicked successfully")
    );
    let typed = text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"type","target":target,"selector":"#name","value":"Robot 世界"}),
    ));
    assert!(
        typed["text"]
            .as_str()
            .expect("DOM text")
            .contains("Robot 世界")
    );
    text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"press","target":target,"key":"Tab"}),
    ));
    let screenshot = call(
        &dir,
        browser.endpoint(),
        json!({"action":"screenshot","target":target}),
    );
    assert_ne!(screenshot["failed"], true, "{screenshot}");
    let png = base64::engine::general_purpose::STANDARD
        .decode(screenshot["images"][0]["data"].as_str().expect("PNG data"))
        .expect("base64");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(png.len() > 1000);
    if let Ok(path) = std::env::var("CASPER_BROWSER_SCREENSHOT") {
        std::fs::write(path, &png).expect("screenshot evidence");
    }
    let scrolled = text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"scroll","target":target,"y":600}),
    ));
    assert!(scrolled["y"].as_i64().expect("scroll position") > 0);
    let next = text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"navigate","target":target,"url":format!("{}/next",site.url)}),
    ));
    assert_eq!(next["title"], "Second page", "{next}");
    text(&call(
        &dir,
        browser.endpoint(),
        json!({"action":"close","target":target}),
    ));
    let tabs = text(&call(&dir, browser.endpoint(), json!({"action":"tabs"})));
    assert!(
        !tabs["tabs"]
            .as_array()
            .expect("tabs")
            .iter()
            .any(|tab| tab["target"] == target)
    );
    drop(browser);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert!(!profile.exists(), "owned profile is removed");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !active_browser_processes(&profile).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        active_browser_processes(&profile).is_empty(),
        "no owned Chromium helper stays running"
    );
}

#[tokio::test]
#[ignore = "requires the pinned Chromium runtime; run oslo make test-browser"]
async fn killing_the_mcp_parent_still_stops_chromium_and_removes_its_profile() {
    let dir = fixture();
    let mut command: tokio::process::Command = command(&dir).into();
    command.args(["mcp", "--tools=browse", "--browser", &program()]);
    if no_sandbox() {
        command.arg("--no-sandbox");
    }
    let transport = TokioChildProcess::new(command).expect("owned browser transport");
    let pid = transport.id().expect("MCP parent pid");
    let client = ().serve(transport).await.expect("owned browser initializes");
    assert!(
        !active_browser_processes(&dir.join("tmp")).is_empty(),
        "real owned Chromium is running"
    );
    assert!(
        std::fs::read_dir(dir.join("tmp"))
            .expect("browser profile exists")
            .next()
            .is_some()
    );
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid as i32).expect("parent pid"),
        rustix::process::Signal::KILL,
    )
    .expect("kill only this test's MCP parent");
    let _ = client.cancel().await;
    let deadline = Instant::now() + Duration::from_secs(8);
    while (std::fs::read_dir(dir.join("tmp"))
        .expect("profile directory")
        .next()
        .is_some()
        || !active_browser_processes(&dir.join("tmp")).is_empty())
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(
        std::fs::read_dir(dir.join("tmp"))
            .expect("profile cleanup")
            .next()
            .is_none(),
        "the parent-death supervisor cleans its owned profile"
    );
    assert!(
        active_browser_processes(&dir.join("tmp")).is_empty(),
        "no owned Chromium helpers remain after parent death"
    );
}

#[tokio::test]
#[ignore = "requires the pinned Chromium runtime; run oslo make test-browser"]
async fn mcp_owns_chromium_and_returns_real_image_content() {
    let dir = fixture();
    let site = Site::new();
    let mut command: tokio::process::Command = command(&dir).into();
    command
        .args(["mcp", "--tools=browse", "--browser", &program()])
        .env("CASPER_CONFIGURE", r#"{"browser":{"allow_private":true}}"#);
    if no_sandbox() {
        command.arg("--no-sandbox");
    }
    let client =
        ().serve(TokioChildProcess::new(command).expect("MCP browser transport"))
            .await
            .expect("MCP browser starts");
    let tools = client.list_all_tools().await.expect("browse schema");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "browse");
    let call = |value: Value| {
        CallToolRequestParams::new("browse")
            .with_arguments(value.as_object().expect("arguments").clone())
    };
    let opened = client
        .call_tool(call(json!({"action":"open","url":site.url})))
        .await
        .expect("MCP open");
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let page: Value = serde_json::from_str(&opened.content[0].as_text().expect("snapshot").text)
        .expect("snapshot JSON");
    assert_eq!(page["title"], "Casper browser fixture");
    let screenshot = client
        .call_tool(call(json!({"action":"screenshot","target":page["target"]})))
        .await
        .expect("MCP screenshot");
    assert_eq!(screenshot.is_error, Some(false), "{screenshot:?}");
    let image = screenshot.content[1].as_image().expect("MCP image block");
    assert_eq!(image.mime_type, "image/png");
    assert!(
        base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .expect("MCP PNG")
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    client.cancel().await.expect("MCP closes");
    let deadline = Instant::now() + Duration::from_secs(8);
    while std::fs::read_dir(dir.join("tmp"))
        .expect("owned profile directory")
        .next()
        .is_some()
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(
        std::fs::read_dir(dir.join("tmp"))
            .expect("profile cleanup")
            .next()
            .is_none(),
        "MCP shutdown cleans its browser profile: {:?}",
        std::fs::read_dir(dir.join("tmp"))
            .expect("profile directory")
            .map(|entry| entry.expect("directory entry").path())
            .collect::<Vec<_>>()
    );
}
