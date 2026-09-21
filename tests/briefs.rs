//! What each shipped file tool says to show once its result is elided, how to get the result back,
//! and that a failure is never elided. `shell`'s own are in `shell.rs`, where a command runs alone.

use casper::lua::engine::Engine;
use casper::scratch::Scratch;
use casper::tools::Ran;

fn call(tool: &str, args: serde_json::Value) -> Ran {
    let mut engine = Engine::new();
    engine
        .run(include_str!("../config/tools.lua"), "tools.lua")
        .expect("the shipped declarations load");
    engine.call(tool, &args).expect("the tool exists")
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
