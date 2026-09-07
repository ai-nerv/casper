//! Every setting casper declares must change something.
//!
//! **A setting advertised in `needs` and read by nothing is worse than one not offered.** A
//! coordinator sets it, is told it was taken, and the behaviour never changes — which is the same
//! sin as a verb that is advertised and refused, one level down. casper declared three settings
//! and honoured one of them: `is_off` was dead code that nothing called, and `output_bytes` was
//! named in its own description and in a test and nowhere else.
//!
//! Driven through the binary rather than the library, because the channel is the whole point:
//! casper is one process per call, so a `configure` that only reached the configuring process
//! reported `set` for something that evaporated on exit.

use std::process::Command;

/// A config directory holding this checkout's declarations.
///
/// **Pointed at rather than inherited.** These spawn the binary, and the binary reads its
/// declarations from `$XDG_CONFIG_HOME/casper` — so without this they would pass on a machine
/// where somebody had run `make install` and fail on a runner where nobody had, which is being
/// green for a reason that has nothing to do with what is being tested.
fn installed() -> casper::scratch::Scratch {
    let dir = casper::scratch::Scratch::new("casper-settings", "config");
    let into = dir.join("casper");
    std::fs::create_dir_all(&into).expect("mkdir");
    std::fs::write(into.join("tools.lua"), include_str!("../config/tools.lua")).expect("write");
    dir
}

/// Run casper with a configuration, and give back stdout.
fn with(configured: &str, args: &[&str], stdin: Option<&str>) -> String {
    let dir = installed();
    let mut command = Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .args(args)
        .env("CASPER_CONFIGURE", configured)
        .env("XDG_CONFIG_HOME", &*dir);
    let Some(body) = stdin else {
        let out = command.output().expect("casper runs");
        return String::from_utf8_lossy(&out.stdout).into_owned();
    };
    use std::io::Write;
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("casper runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(body.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("casper finishes");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_coordinator_reaches_a_program_it_spawns_per_call() {
    // The hole this file exists for. `casper configure` applied to the process running it and
    // nothing else, so magi could configure casper all it liked and every later `casper run` was
    // a fresh process that knew nothing about it.
    let listed = with(r#"{"tools":{"dino":{"off":true}}}"#, &["tools"], None);
    assert!(!listed.contains(r#""name":"dino""#), "{listed}");
    let plain = with("", &["tools"], None);
    assert!(
        plain.contains(r#""name":"dino""#),
        "and it is there by default"
    );
}

#[test]
fn off_means_gone_rather_than_unlisted() {
    // A model that was never told about a tool can still guess at one. Answering the guess would
    // make `off` mean `hidden` for anything persistent enough to try.
    let ran = with(
        r#"{"tools":{"dino":{"off":true}}}"#,
        &["run"],
        Some(r#"{"tool":"dino","args":{}}"#),
    );
    assert!(ran.contains(r#""ok":false"#), "{ran}");
    assert!(ran.contains("no such tool: dino"), "{ran}");
}

#[test]
fn hidden_takes_a_tool_out_of_the_listing_and_leaves_it_runnable() {
    // The other half, and the reason there are two settings: a tool a person invokes through the
    // harness should not spend context in every request that mentions it.
    let listed = with(r#"{"tools":{"pwd":{"hidden":true}}}"#, &["tools"], None);
    assert!(!listed.contains(r#""name":"pwd""#), "{listed}");

    let ran = with(
        r#"{"tools":{"pwd":{"hidden":true}}}"#,
        &["run"],
        Some(r#"{"tool":"pwd","args":{}}"#),
    );
    assert!(ran.contains(r#""ok":true"#), "still runs: {ran}");
}

#[test]
fn output_bytes_caps_what_the_model_reads() {
    // Declared with a default since `needs` existed, and applied to nothing.
    let ran = with(
        r#"{"output_bytes":200}"#,
        &["run"],
        Some(r#"{"tool":"cat","args":{"path":"config/tools.lua"}}"#),
    );
    let reply: serde_json::Value = serde_json::from_str(&ran).unwrap_or_else(|_| panic!("{ran}"));
    let said = reply["result"][0]["said"].as_str().expect("said");
    assert!(
        said.len() < 400,
        "cut to about the cap: {} bytes",
        said.len()
    );
    assert!(said.contains("bytes dropped"), "and says so: {said}");
    // Both ends kept: a file read wants its head, a build that failed wants its tail.
    assert!(said.starts_with("-- The tools casper offers."), "{said}");
    assert!(said.trim_end().ends_with("end"), "{said}");
}

#[test]
fn every_declared_setting_is_read_by_something() {
    // The rule this file enforces, stated once. If a fourth setting is added to `needs`, this
    // fails until somebody writes the test that proves it does something.
    let covered = ["tools", "load", "output_bytes"];
    for need in casper::setup::needs() {
        assert!(
            covered.contains(&need.name.as_str()),
            "`{}` is declared in `needs` and nothing here proves it changes anything",
            need.name
        );
    }
}
