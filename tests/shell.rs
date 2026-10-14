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
        self.asking_in(call, None, &[])
    }

    /// One call from casper started in `project`, with `env` on top.
    fn asking_in(
        &self,
        call: &str,
        project: Option<&std::path::Path>,
        env: &[(&str, std::ffi::OsString)],
    ) -> serde_json::Value {
        let mut command = Command::new(CASPER);
        if let Some(project) = project {
            command.current_dir(project);
        }
        let done = command
            .arg("run")
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_RUNTIME_DIR", &*self.dir)
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .envs(env.iter().map(|(key, value)| (key, value)))
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

    /// The same, from casper started in `project` with `env`; answers with what it printed.
    fn shell_in(
        &self,
        project: &std::path::Path,
        env: &[(&str, std::ffi::OsString)],
        command: &str,
    ) -> String {
        let call = serde_json::json!({"tool": "shell", "args": {"command": command}});
        let said = self.asking_in(&call.to_string(), Some(project), env)["result"][0].clone();
        said["said"].as_str().unwrap_or_default().to_owned()
    }

    /// A project directory with a `sub` directory in it.
    fn project(&self, name: &str) -> std::path::PathBuf {
        let project = self.dir.join(name);
        std::fs::create_dir_all(project.join("sub")).expect("project");
        std::fs::canonicalize(project).expect("canonical project")
    }
}

#[test]
fn a_command_says_how_it_ended_and_what_it_said_last_for_its_stub() {
    let alone = Alone::new("brief");
    let said = alone.shell("printf 'one\\ntwo\\n'");
    let brief = said["brief"].as_str().unwrap_or_default();
    assert!(brief.contains("exited 0, 2 lines"), "{said}");
    assert!(brief.contains(r#"last: "two""#), "{said}");
    assert_eq!(said["back"], "shell: printf 'one\\ntwo\\n'", "{said}");
    assert!(
        said.get("keep").is_none(),
        "a success may be elided: {said}"
    );
}

#[test]
fn a_command_that_failed_is_kept_with_its_status_as_the_stub() {
    let alone = Alone::new("kept");
    let said = alone.shell("echo broke >&2; sh -c 'exit 3'");
    assert_eq!(said["keep"], serde_json::json!(true), "{said}");
    assert!(
        said["brief"]
            .as_str()
            .unwrap_or_default()
            .contains("exited 3"),
        "{said}"
    );
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
}

#[test]
fn a_cd_in_one_project_does_not_move_another() {
    let alone = Alone::new("projects");
    let (one, two) = (alone.project("one"), alone.project("two"));
    alone.shell_in(&one, &[], "cd sub");
    assert_eq!(
        alone.shell_in(&two, &[], "pwd"),
        format!("{}\n", two.display())
    );
    let moved = one.join("sub");
    assert_eq!(
        alone.shell_in(&one, &[], "pwd"),
        format!("{}\n", moved.display())
    );
}

#[test]
fn a_remembered_directory_is_never_read_as_shell() {
    let alone = Alone::new("planted");
    let project = alone.project("p");
    let name = "x$(touch ran)`touch ran`";
    std::fs::create_dir(project.join(name)).expect("planted directory");
    alone.shell_in(&project, &[], &format!("cd '{name}'"));
    let at = alone.shell_in(&project, &[], "pwd");
    assert_eq!(at, format!("{}\n", project.join(name).display()));
    assert!(!project.join("ran").exists() && !project.join(name).join("ran").exists());
}

/// A directory of links to the programs the shell script needs, and no `bwrap` among them.
fn without_bubblewrap(alone: &Alone) -> std::ffi::OsString {
    let bin = alone.dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let path = std::env::var_os("PATH").expect("PATH");
    for tool in ["sh", "cksum", "cut", "id", "mkdir", "chmod", "cat"] {
        let found = std::env::split_paths(&path)
            .map(|dir| dir.join(tool))
            .find(|candidate| candidate.is_file())
            .expect("a program the shell script needs");
        std::os::unix::fs::symlink(found, bin.join(tool)).expect("link");
    }
    bin.into_os_string()
}

#[test]
fn a_jailed_shell_remembers_its_directory_in_the_jails_own_tmp() {
    let alone = Alone::new("jailed");
    let (tmp, home) = (alone.dir.join("tmp"), alone.dir.join("home"));
    for dir in [&tmp, &home] {
        std::fs::create_dir_all(dir).expect("fixture");
    }
    let jail = serde_json::json!({ "tmp": tmp }).to_string();
    let paths = [
        ("bubblewrap", std::env::var_os("PATH").expect("PATH")),
        ("landlock", without_bubblewrap(&alone)),
    ];
    for (backend, path) in paths {
        let project = alone.project(backend);
        let env = [
            ("CASPER_JAIL", jail.clone().into()),
            ("HOME", home.clone().into_os_string()),
            ("PATH", path),
        ];
        alone.shell_in(&project, &env, "cd sub");
        let at = alone.shell_in(&project, &env, "pwd");
        assert_eq!(
            at,
            format!("{}\n", project.join("sub").display()),
            "{backend}"
        );
    }
}
