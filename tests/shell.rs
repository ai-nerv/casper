//! What the `shell` declaration owes its caller besides ending when casper does.
//!
//! Against the real binary and the `tools.lua` this repository ships: `shell` does not run the
//! command, it writes a shell script around it, and the exit status and the remembered directory
//! are properties of that script rather than of any Rust in the crate. The script backgrounds the
//! command to stop leaking processes (see `tests/tied.rs`), so the status is checked in both
//! directions and so is the `cd` that has to survive to the next call.

use casper::scratch::Scratch;
use std::process::{Command, Stdio};

const CASPER: &str = env!("CARGO_BIN_EXE_casper");

/// One casper, with config, runtime and data directories of its own, so the test reads this
/// repository's declarations and not the machine's.
struct Alone {
    dir: Scratch,
}

impl Alone {
    fn new(name: &str) -> Self {
        let dir = Scratch::new("casper-shell", name);
        let config = dir.join("config/casper");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config/tools.lua"),
            config.join("tools.lua"),
        )
        .expect("the shipped declarations");
        Self { dir }
    }

    /// Make one call, the way the harness makes them: JSON on stdin, JSON on stdout.
    fn asking(&self, call: &str) -> serde_json::Value {
        let done = Command::new(CASPER)
            .arg("run")
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_RUNTIME_DIR", &*self.dir)
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut casper| {
                use std::io::Write as _;
                casper
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(format!("{call}\n").as_bytes())?;
                casper.wait_with_output()
            })
            .expect("casper ran");
        serde_json::from_slice(&done.stdout).expect("a reply in the family's shape")
    }

    /// Run `command` through `shell`, and answer with the one result it came back with.
    fn shell(&self, command: &str) -> serde_json::Value {
        let call = serde_json::json!({"tool": "shell", "args": {"command": command}});
        self.asking(&call.to_string())["result"][0].clone()
    }
}

#[test]
fn a_command_that_failed_is_reported_with_the_status_it_failed_with() {
    // `sh -c` rather than the `exit` builtin, so the status crosses a real process.
    let alone = Alone::new("failed");
    let said = alone.shell("echo working; echo broken >&2; sh -c 'exit 3'");
    assert_eq!(said["failed"], serde_json::json!(true), "{said}");
    let text = said["said"].as_str().unwrap_or_default();
    assert!(text.contains("(exit 3)"), "{text}");
    assert!(
        text.contains("working") && text.contains("broken"),
        "{text}"
    );
}

#[test]
fn a_command_that_worked_is_not_reported_as_a_failure() {
    let alone = Alone::new("worked");
    let said = alone.shell("echo all good");
    assert_eq!(said["failed"], serde_json::Value::Null, "{said}");
    assert_eq!(said["said"], serde_json::json!("all good\n"));
}

#[test]
fn the_directory_a_command_ended_in_is_where_the_next_one_starts() {
    // A `cd` in a subshell is not a `cd` in the shell that writes the directory down.
    let alone = Alone::new("cd");
    alone.shell("cd /usr/share");
    assert_eq!(
        alone.shell("pwd")["said"],
        serde_json::json!("/usr/share\n")
    );
    assert_eq!(
        alone.asking(r#"{"tool":"pwd","args":{}}"#)["result"][0]["said"],
        serde_json::json!("/usr/share")
    );
}
