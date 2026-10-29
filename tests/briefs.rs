//! What each shipped file tool says to show once its result is elided, how to get the result back,
//! and that a failure is never elided. `shell`'s own are in `shell.rs`, where a command runs alone.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::scratch::Scratch;
use casper::tools::Ran;
use std::io::Write;
use std::process::Stdio;

#[path = "support/native_namespace.rs"]
mod native_namespace;
use native_namespace::NativeNamespace;

fn call(tool: &str, args: serde_json::Value) -> Ran {
    let dir = Scratch::new("casper-briefs", "config");
    let config = dir.join("config/casper");
    std::fs::create_dir_all(&config).expect("private config");
    std::fs::write(
        config.join("tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("shipped tools");
    let binary = std::env::var_os("CASPER_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_casper").into());
    let path = std::path::Path::new(args["path"].as_str().expect("file path"));
    let namespace = NativeNamespace::new(&dir, path.parent().expect("fixture workspace"));
    let mut request = serde_json::json!({"tool":tool,"args":args,
        "file_revision":{"version":1,"operation":tool}});
    if tool == "write" || tool == "edit" {
        let expected = if tool == "edit" {
            let read = exchange(
                &namespace,
                &binary,
                &serde_json::json!({
                "tool":"read","args":{"path":request["args"]["path"]},
                "file_revision":{"version":1,"operation":"read"}}),
            );
            assert_eq!(read["ok"], true, "{read}");
            read["result"][0]["observations"][0]["revision"]
                .as_str()
                .expect("native receipt")
                .to_owned()
        } else {
            "absent".to_owned()
        };
        request["revision_contract"] = serde_json::json!(1);
        request["expected_revision"] = serde_json::json!(expected);
        request["mutation_id"] = serde_json::json!("briefs-mutation");
    }
    let reply = exchange(&namespace, &binary, &request);
    assert_eq!(reply["ok"], true, "{reply}");
    serde_json::from_value(reply["result"][0].clone()).expect("tool result")
}

fn exchange(
    namespace: &NativeNamespace,
    binary: &std::ffi::OsStr,
    request: &serde_json::Value,
) -> serde_json::Value {
    let mut child = namespace
        .command("timeout")
        .args(["--kill-after=2s", "45s"])
        .arg(binary)
        .arg("run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tool process");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&serde_json::to_vec(request).expect("request"))
        .expect("send");
    let output = child.wait_with_output().expect("tool result");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("reply")
}

#[test]
fn a_read_says_which_file_and_how_long_it_is() {
    let dir = Scratch::new("casper-briefs", "read");
    let path = dir.join("a.rs");
    std::fs::write(&path, "fn a() {}\nfn b() {}\nfn c() {}\n").expect("written");
    let path = path.display().to_string();
    let ran = call("read", serde_json::json!({"path": path}));
    assert_eq!(
        ran.brief.as_deref(),
        Some(format!("read {path} (3 lines)").as_str())
    );
    assert_eq!(ran.back.as_deref(), Some(format!("read {path}").as_str()));
    assert!(!ran.keep);
}

#[test]
fn a_write_says_where_it_went_and_whether_it_was_new() {
    let dir = Scratch::new("casper-briefs", "write");
    let path = dir.join("new.py").display().to_string();
    let ran = call(
        "write",
        serde_json::json!({"path": path, "contents": "x = 1\ny = 2\n"}),
    );
    assert_eq!(
        ran.brief.as_deref(),
        Some(format!("wrote {path} (2 lines, new file)").as_str())
    );
    assert_eq!(ran.back.as_deref(), Some(format!("read {path}").as_str()));
}

#[test]
fn an_edit_counts_what_it_added_and_removed() {
    let dir = Scratch::new("casper-briefs", "edit");
    let path = dir.join("e.txt");
    std::fs::write(&path, "a\nb\nc\n").expect("written");
    let path = path.display().to_string();
    let ran = call(
        "edit",
        serde_json::json!({"path": path, "old": "b", "new": "B\nB2"}),
    );
    assert_eq!(
        ran.brief.as_deref(),
        Some(format!("edited {path} (+2 -1 at line 2)").as_str())
    );
    assert_eq!(ran.back.as_deref(), Some(format!("read {path}").as_str()));
}

#[test]
fn a_failure_is_kept_whole_with_its_first_line_as_the_stub() {
    let dir = Scratch::new("casper-briefs", "fail");
    let missing = dir.join("nope.txt").display().to_string();
    let ran = call("read", serde_json::json!({"path": missing}));
    assert!(ran.failed && ran.keep, "{ran:?}");
    assert!(!ran.brief.unwrap_or_default().is_empty());
}
