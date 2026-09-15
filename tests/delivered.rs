//! What casper's exit code says about whether the caller got what casper wrote.
//!
//! Against the real binary, because the thing under test is a buffered `stdout` and a process's
//! exit code. `/dev/full` is the fixture: it accepts an `open` and reports `ENOSPC` on every
//! write. Not a pipe — a kilobyte fits in a 64 KiB pipe buffer and succeeds whether or not
//! anybody reads it, which passes against the broken version too.
//!
//! Every case also asserts that nothing panicked, because `println!` and `eprintln!` panic on a
//! failed write and exit 101 is not exit 1.

use casper::scratch::Scratch;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const CASPER: &str = env!("CARGO_BIN_EXE_casper");

/// `/dev/full` opened for writing: every write to it is `ENOSPC`.
fn full() -> std::fs::File {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full")
}

/// Run one verb with stdout on `/dev/full`, keeping stderr to check for a panic.
fn to_a_full_stdout(args: &[&str]) -> Output {
    Command::new(CASPER)
        .args(args)
        .stdin(Stdio::null())
        .stdout(full())
        .stderr(Stdio::piped())
        .output()
        .expect("casper runs")
}

/// Whether a run died through the panic handler rather than reporting.
fn panicked(out: &Output) -> String {
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!said.contains("panicked"), "{said}");
    said
}

#[test]
fn a_json_reply_that_did_not_land_is_not_a_success() {
    let out = to_a_full_stdout(&["verbs"]);
    panicked(&out);
    assert!(!out.status.success());
}

#[test]
fn a_cbor_reply_that_did_not_land_is_not_a_success_either() {
    // No trailing newline, so `LineWriter` holds it and `write_all` reports success.
    let out = to_a_full_stdout(&["--cbor", "verbs"]);
    panicked(&out);
    assert!(!out.status.success());
}

#[test]
fn help_that_did_not_land_is_not_a_success() {
    // `usage` printed with `println!`, which panics on a failed write.
    let out = to_a_full_stdout(&["help"]);
    panicked(&out);
    assert!(!out.status.success());
}

#[test]
fn a_reply_that_landed_is_a_success() {
    // The control for the cases above, which pass against a casper that always fails.
    let out = Command::new(CASPER)
        .arg("verbs")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("casper runs");
    assert!(out.status.success(), "a reply nobody interfered with");
    assert!(!out.stdout.is_empty(), "and there was one");
}

#[test]
fn a_refusal_still_exits_zero() {
    // A refusal is an answer. Only a reply that never arrived is a failure.
    let out = Command::new(CASPER)
        .arg("no-such-verb")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("casper runs");
    assert!(out.status.success(), "a refusal is not a failure");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"ok\":false"),
        "and it is still a refusal"
    );
}

/// A configuration directory of this test's own, holding the declarations this repository ships
/// rather than the machine's, and optionally a `plugin/` file that will not parse.
fn config(name: &str, broken: bool) -> Scratch {
    let dir = Scratch::new("casper-delivered", name);
    let into = dir.join("config/casper");
    std::fs::create_dir_all(into.join("plugin")).expect("mkdir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("config/tools.lua"),
        into.join("tools.lua"),
    )
    .expect("the shipped declarations");
    if broken {
        std::fs::write(into.join("plugin/unparseable.lua"), "this is not lua (((\n")
            .expect("wrote");
    }
    dir
}

/// Run `args` against `dir`'s declarations, with the environment pointed inside it.
fn against(dir: &Scratch, args: &[&str]) -> Command {
    let mut command = Command::new(CASPER);
    command
        .args(args)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_RUNTIME_DIR", dir.join("runtime"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .stdin(Stdio::null());
    command
}

#[test]
fn a_diagnostic_that_cannot_be_written_does_not_cost_the_reply() {
    // `eprintln!` panics on a failed write, so a full stderr turned a named broken declaration
    // into exit 101 and an empty stdout.
    let dir = config("aside", true);
    let out = against(&dir, &["tools"])
        .stderr(full())
        .output()
        .expect("casper runs");
    assert!(
        out.status.success(),
        "a layer that would not parse is not a failed call"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"ok\":true"),
        "the reply still arrived: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_broken_layer_is_still_named_when_stderr_works() {
    // The control for the case above: the note is dropped only when it cannot be delivered.
    let dir = config("named", true);
    let out = against(&dir, &["tools"])
        .stderr(Stdio::piped())
        .output()
        .expect("casper runs");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unparseable.lua"),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_surface_whose_frames_did_not_land_is_not_a_success() {
    // A surface wrote its frames with `println!`, which panics on a failed write.
    let dir = config("surface", false);
    let mut running = against(&dir, &["surface", "dino"])
        .stdin(Stdio::piped())
        .stdout(full())
        .stderr(Stdio::piped())
        .spawn()
        .expect("casper runs");
    {
        use std::io::Write;
        let mut asked = running.stdin.take().expect("stdin");
        asked
            .write_all(br#"{"event":"open","rows":12,"cols":80,"holds":false,"args":{}}"#)
            .and_then(|()| asked.write_all(b"\n"))
            .expect("the open frame");
    }
    let out = running.wait_with_output().expect("casper ends");
    panicked(&out);
    assert!(
        !out.status.success(),
        "a surface nobody received is not a surface that was drawn"
    );
}

#[test]
fn a_surface_whose_frames_landed_is_a_success() {
    // The control. `dino` draws on the open frame and ends when stdin closes.
    let dir = config("drawn", false);
    let out = against(&dir, &["surface", "dino"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut running| {
            use std::io::Write;
            let mut asked = running.stdin.take().expect("stdin");
            asked.write_all(br#"{"event":"open","rows":12,"cols":80,"holds":false,"args":{}}"#)?;
            asked.write_all(b"\n")?;
            drop(asked);
            running.wait_with_output()
        })
        .expect("casper runs");
    assert!(out.status.success(), "{:?}", panicked(&out));
    assert!(!out.stdout.is_empty(), "and it drew something");
}

/// `casper configure` with `source` on stdin, in directories of this test's own. `log` is where
/// `$CASPER_DEBUG_LOG` points, if anywhere.
fn configured(dir: &Scratch, source: &str, err: Stdio, log: Option<&Path>) -> Output {
    let mut command = Command::new(CASPER);
    command
        .arg("configure")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_RUNTIME_DIR", dir.join("runtime"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(err);
    if let Some(log) = log {
        command.env(casper::noted::VARIABLE, log);
    }
    let mut running = command.spawn().expect("casper runs");
    {
        use std::io::Write;
        let mut asked = running.stdin.take().expect("stdin");
        asked.write_all(source.as_bytes()).expect("the chunk");
    }
    running.wait_with_output().expect("casper ends")
}

#[test]
fn a_declarations_print_does_not_go_out_on_the_wire() {
    // luna's `print` writes to stdout, and stdout is the reply: the diagnostic arrived ahead of
    // it and the caller read it as the answer, at exit 0.
    let dir = Scratch::new("casper-delivered", "printed");
    let out = configured(&dir, r#"print("WIRE-CORRUPTION")"#, Stdio::piped(), None);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}", panicked(&out));
    assert!(!said.contains("WIRE-CORRUPTION"), "on the wire: {said:?}");
    assert_eq!(
        said.lines().count(),
        1,
        "one reply and nothing else: {said:?}"
    );
    assert!(said.contains("\"ok\":true"), "{said:?}");
}

#[test]
fn a_declarations_warning_that_cannot_be_written_does_not_cost_the_reply() {
    // luna's `warn` goes through `eprintln!`, which panics on a failed write. This was exit 101
    // with an empty stdout, from one line in a declaration.
    let dir = Scratch::new("casper-delivered", "warned");
    let out = configured(&dir, r#"warn("boom")"#, full().into(), None);
    assert!(
        out.status.success(),
        "a warning nobody could write is not a failed call"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"ok\":true"),
        "the reply still arrived: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_declarations_diagnostic_still_reaches_the_debug_log() {
    // The control for both: neither is thrown away, they go where casper's own notes go.
    let dir = Scratch::new("casper-delivered", "logged");
    let log = dir.join("notes.txt");
    let out = configured(
        &dir,
        r#"print("said", 42) warn("also")"#,
        Stdio::piped(),
        Some(&log),
    );
    assert!(out.status.success(), "{:?}", panicked(&out));
    let held = std::fs::read_to_string(&log).expect("the log");
    assert!(held.contains("print: said\t42"), "{held:?}");
    assert!(held.contains("warn: also"), "{held:?}");
}

/// A surface on a machine with no declarations still says it is finished: the harness reserves
/// the rows before this process starts and holds them until something says otherwise.
#[test]
fn a_surface_with_nothing_to_draw_still_says_so() {
    let dir = Scratch::new("casper-delivered", "empty");
    std::fs::create_dir_all(dir.join("config/casper")).expect("mkdir");
    let out = against(&dir, &["surface", "dino"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("casper runs");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"done\""),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
