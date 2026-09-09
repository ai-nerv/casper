//! What the `shell` declaration owes its caller besides ending when casper does.
//!
//! Against the real binary and the `tools.lua` this repository ships, because the wrapper is the
//! thing under test: `shell` does not run the command, it writes a shell script around it, and
//! every property here — the exit status, the remembered directory — is a property of that
//! script rather than of any Rust in the crate. A test that built its own `Command` would agree
//! with itself and say nothing about what a model gets back.
//!
//! The reason to hold it down at all is that the script had to change to stop leaking processes
//! (see `tests/tied.rs`), and the change moved the command into a background job. **A tool that
//! answers 0 for a command that failed is worse than the leak it was fixed for**, so the status
//! is checked in both directions, and so is the `cd` that has to survive to the next call.

use std::process::{Command, Stdio};

/// The binary under test.
const CASPER: &str = env!("CARGO_BIN_EXE_casper");

/// One casper, with a config and a runtime directory of its own.
///
/// Both are isolated for the same reason: casper reads `tools.lua` out of its config directory,
/// so without this the test would report on whatever is installed on the machine rather than on
/// what is in the repository — and `shell` writes the directory it is to use next into the
/// runtime directory, which on a developer's machine is one a person is using.
struct Alone {
    dir: std::path::PathBuf,
}

impl Alone {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("casper-shell-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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
            .env("XDG_RUNTIME_DIR", &self.dir)
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

impl Drop for Alone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_command_that_failed_is_reported_with_the_status_it_failed_with() {
    // The number, not merely "something went wrong": 3 is what the program chose to say, and a
    // model that gets 1 or 0 instead is being told about a different run than the one it asked
    // for. `sh -c` rather than the `exit` builtin so that the status crosses a real process.
    let alone = Alone::new("failed");
    let said = alone.shell("echo working; echo broken >&2; sh -c 'exit 3'");
    assert_eq!(said["failed"], serde_json::json!(true), "{said}");
    let text = said["said"].as_str().unwrap_or_default();
    assert!(text.contains("(exit 3)"), "{text}");
    // And what it printed on both streams, which is usually what says how to fix it.
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
    // The one piece of state `shell` keeps, and the first thing a background job would have
    // broken: a `cd` in a subshell is not a `cd` in the shell that writes the directory down.
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
