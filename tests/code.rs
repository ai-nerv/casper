//! `code`: the verb against a recorded skeleton per language, the tool end to end through each jail
//! backend, and what it refuses.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::scratch::Scratch;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/code/golden");

/// The grammars the suite parses with, as `CASPER_GRAMMARS` carries them: its own value, or where
/// `make grammars` builds them. Every pinned language must be built there.
fn grammars() -> OsString {
    let named = std::env::var_os("CASPER_GRAMMARS")
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/target/grammars").into());
    let shelf = casper::shelf::Shelf::at(std::env::split_paths(&named).collect());
    let missing: Vec<&str> = casper::grammars::lock()
        .grammars
        .keys()
        .map(String::as_str)
        .filter(|language| shelf.find(language).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "no grammar built for {}: run `oslo make grammars`, which fetches and builds them into \
         target/grammars, or name a directory holding them in CASPER_GRAMMARS",
        missing.join(" ")
    );
    named
}

/// What `casper code` answers for `path`, run from `cwd` with no jail.
fn verb(cwd: &Path, path: &Path) -> Value {
    verb_with(cwd, path, &grammars())
}

/// [`verb`], finding its grammars in `named` alone.
fn verb_with(cwd: &Path, path: &Path, named: &OsStr) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_casper"))
        .arg("code")
        .arg(path)
        .current_dir(cwd)
        .env_remove("CASPER_CONFIGURE")
        .env_remove("CASPER_JAIL")
        .env("CASPER_GRAMMARS", named)
        .env("XDG_DATA_HOME", "/nonexistent")
        .output()
        .expect("casper");
    serde_json::from_slice(&out.stdout).expect("a reply")
}

/// A working tree, a configuration holding the shipped declarations, and an empty `bin`.
fn fixture(name: &str) -> Scratch {
    let dir = Scratch::new("code", name);
    for path in ["config/casper", "work", "home", "bin"] {
        std::fs::create_dir_all(dir.join(path)).expect("fixture directory");
    }
    std::fs::write(
        dir.join("config/casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("declarations");
    dir
}

/// Which jail a call runs in. `PATH` holds only the fixture's `bin`, so bubblewrap is used when a
/// link to it is put there, and Landlock when not.
#[derive(Clone, Copy, Debug)]
enum Jail {
    Off,
    Bubblewrap,
    Landlock,
}

/// The `code` tool's result for `args`, through `casper run`.
fn tool(dir: &Scratch, args: Value, jail: Jail) -> Value {
    tool_with(dir, args, jail, &grammars())
}

/// [`tool`], finding its grammars in `named` alone.
fn tool_with(dir: &Scratch, args: Value, jail: Jail, named: &OsStr) -> Value {
    use std::io::Write as _;
    let mut command = Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .arg("run")
        .current_dir(dir.join("work"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("HOME", dir.join("home"))
        .env("CASPER_GRAMMARS", named)
        .env_remove("CASPER_CONFIGURE")
        .env_remove("CASPER_JAIL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if !matches!(jail, Jail::Off) {
        command.env("CASPER_JAIL", "1").env("PATH", dir.join("bin"));
    }
    let mut child = command.spawn().expect("casper");
    let call = json!({"tool": "code", "args": args}).to_string();
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(call.as_bytes())
        .expect("call");
    let out = child.wait_with_output().expect("output");
    let reply: Value = serde_json::from_slice(&out.stdout).expect("a reply");
    reply["result"][0].clone()
}

fn installed(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// Whether a backend this machine lacks may be skipped rather than failed.
fn excused(what: &str) -> bool {
    assert!(
        std::env::var_os("CASPER_REQUIRE_CONTAINMENT").is_none(),
        "required {what} unavailable"
    );
    eprintln!("NOT VERIFIED: {what} unavailable");
    true
}

#[test]
fn every_language_indexes_its_golden_file_to_the_recorded_skeleton() {
    let mut languages = BTreeSet::new();
    for entry in std::fs::read_dir(GOLDEN).expect("golden files") {
        let input = entry.expect("an entry").path();
        if input.extension().is_some_and(|ext| ext == "skeleton") {
            continue;
        }
        let recorded = std::fs::read_to_string(format!("{}.skeleton", input.display()))
            .expect("a recorded skeleton");
        let reply = verb(Path::new(GOLDEN), &input);
        assert_eq!(reply["ok"], true, "{}: {reply}", input.display());
        let found = &reply["result"][0];
        assert_eq!(
            found["skeleton"].as_str().map(|said| format!("{said}\n")),
            Some(recorded),
            "{}",
            input.display()
        );
        languages.insert(found["language"].as_str().unwrap_or_default().to_owned());
    }
    let every = "bash bazel_build bazel_bzl bazel_module c c_sharp clojure containerfile cpp css \
                 dart elixir gleam go hcl html java javascript json kotlin lua_lang make markdown \
                 nix php python ruby rust scala sql swift toml tsx typescript v yaml zig";
    assert_eq!(
        languages,
        every.split_whitespace().map(str::to_owned).collect()
    );
}

#[test]
fn the_tool_indexes_inside_the_jail_and_nothing_past_it() {
    let outside = Scratch::new("code-secret", "outside");
    std::fs::write(outside.join("secret.rs"), "fn secret() {}\n").expect("outside");
    for jail in [Jail::Off, Jail::Bubblewrap, Jail::Landlock] {
        let dir = fixture(&format!("{jail:?}"));
        if let Jail::Bubblewrap = jail {
            let Some(bwrap) = installed("bwrap") else {
                excused("bubblewrap");
                continue;
            };
            std::os::unix::fs::symlink(bwrap, dir.join("bin/bwrap")).expect("bwrap link");
        }
        let file = dir.join("work/lib.rs");
        std::fs::write(&file, "use std::io;\n\npub fn run() {}\n").expect("source");
        std::fs::create_dir(dir.join("work/src")).expect("a directory");
        let ran = tool(&dir, json!({"path": file}), jail);
        if matches!(jail, Jail::Landlock)
            && ran["said"]
                .as_str()
                .is_some_and(|said| said.contains("Landlock"))
        {
            excused("Landlock");
            continue;
        }
        assert_eq!(ran["failed"], Value::Null, "{jail:?}: {ran}");
        assert_eq!(
            ran["said"], "imports: [1]\n  std::io\n\nfns:\n  pub run() [3]",
            "{jail:?}"
        );
        let path = file.display();
        assert_eq!(ran["brief"], format!("code {path} (5 lines)"), "{jail:?}");
        assert_eq!(ran["back"], format!("code {path}"), "{jail:?}");

        let listed = tool(&dir, json!({"path": dir.join("work")}), jail);
        assert_eq!(listed["said"], "src/\nlib.rs", "{jail:?}: {listed}");

        let secret = tool(&dir, json!({"path": outside.join("secret.rs")}), jail);
        let escaped = !matches!(jail, Jail::Off);
        assert_eq!(secret["failed"] == true, escaped, "{jail:?}: {secret}");
        assert_eq!(
            secret["said"]
                .as_str()
                .is_some_and(|said| said.contains("secret()")),
            !escaped,
            "{jail:?}: {secret}"
        );
    }
}

#[test]
fn a_file_type_it_does_not_read_fails_with_what_to_use_instead() {
    let dir = fixture("unsupported");
    std::fs::write(dir.join("work/notes.xyz"), "anything\n").expect("file");
    std::fs::write(dir.join("work/LICENSE"), "MIT\n").expect("file");
    let ran = tool(&dir, json!({"path": "notes.xyz"}), Jail::Off);
    assert_eq!(ran["failed"], true, "{ran}");
    assert_eq!(ran["keep"], true, "{ran}");
    assert_eq!(
        ran["said"],
        "Unsupported file type: .xyz. Use the read tool instead."
    );
    let reply = verb(&dir.join("work"), Path::new("LICENSE"));
    assert_eq!(
        reply["error"],
        "Unsupported file type: (no extension). Use the read tool instead."
    );
}

#[test]
fn a_file_over_two_mebibytes_is_refused_and_one_at_the_limit_is_not() {
    let dir = fixture("large");
    let file = dir.join("work/big.rs");
    std::fs::write(&file, vec![b'\n'; 2 * 1024 * 1024 + 1]).expect("large");
    let reply = verb(&dir.join("work"), &file);
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(
        reply["error"],
        "error: File too large (2097153 bytes, max 2097152). Use read with offset/limit instead."
    );
    let ran = tool(&dir, json!({"path": "big.rs"}), Jail::Off);
    assert_eq!(ran["failed"], true, "{ran}");
    std::fs::write(&file, vec![b'\n'; 2 * 1024 * 1024]).expect("at the limit");
    assert_eq!(verb(&dir.join("work"), &file)["ok"], true);
}

#[test]
fn a_directory_lists_directories_first_then_files_each_sorted() {
    let dir = fixture("listing");
    let work = dir.join("work");
    for file in ["c.txt", "a.txt", "AGENTS.md", "b.txt", "m.txt"] {
        std::fs::write(work.join(file), "").expect("file");
    }
    for sub in ["zdir", "adir", "idir"] {
        std::fs::create_dir(work.join(sub)).expect("directory");
    }
    std::os::unix::fs::symlink(work.join("adir"), work.join("link")).expect("link");
    let reply = verb(&work, Path::new("."));
    assert_eq!(reply["result"][0]["kind"], "directory", "{reply}");
    assert_eq!(
        reply["result"][0]["listing"],
        "adir/\nidir/\nlink/\nzdir/\nAGENTS.md\na.txt\nb.txt\nc.txt\nm.txt"
    );
    let ran = tool(&dir, json!({"path": "."}), Jail::Off);
    assert_eq!(ran["brief"], "code . (9 entries)");
}

#[test]
fn a_missing_path_or_a_pipe_is_named_rather_than_waited_on() {
    let dir = fixture("missing");
    let work = dir.join("work");
    let reply = verb(&work, Path::new("gone.rs"));
    assert_eq!(reply["error"], "error: path not found: gone.rs");
    let made = Command::new("mkfifo")
        .arg(work.join("pipe.rs"))
        .status()
        .expect("mkfifo");
    assert!(made.success());
    let reply = verb(&work, Path::new("pipe.rs"));
    assert_eq!(reply["error"], "error: not a regular file: pipe.rs");
    let reply = verb(&work, Path::new(""));
    assert_eq!(reply["ok"], false, "{reply}");
}

#[test]
fn a_skeleton_too_large_for_one_read_arrives_cut_and_says_so() {
    let dir = fixture("cut");
    let source: String = (1..=3000)
        .map(|n| {
            format!(
                "pub fn function_number_{n}(alpha: u32, beta: &str, gamma: Option<Vec<u8>>) \
                 -> Result<u32, String> {{ todo!() }}\n"
            )
        })
        .collect();
    std::fs::write(dir.join("work/sig.rs"), source).expect("source");
    let ran = tool(&dir, json!({"path": "sig.rs"}), Jail::Off);
    assert_eq!(ran["failed"], Value::Null, "{ran}");
    let said = ran["said"].as_str().unwrap_or_default();
    assert!(
        said.starts_with("fns:\n  pub function_number_1("),
        "{said:.200}"
    );
    assert!(
        said.ends_with("more bytes, not shown"),
        "{}",
        &said[said.len() - 80..]
    );
    assert_eq!(ran["brief"], "code sig.rs (3001 lines)");
}

#[test]
fn a_language_whose_grammar_is_not_built_says_how_to_fetch_it() {
    let dir = fixture("unbuilt");
    std::fs::write(dir.join("work/lib.rs"), "fn a() {}\n").expect("source");
    let notice = "no rust grammar yet: run `casper grammar fetch rust` (or `casper grammar fetch` \
                  for every language)\nthat needs git and a C compiler";
    let reply = verb_with(
        &dir.join("work"),
        Path::new("lib.rs"),
        dir.join("bin").as_os_str(),
    );
    assert_eq!(reply["error"], notice, "{reply}");
    for jail in [Jail::Off, Jail::Landlock] {
        let ran = tool_with(
            &dir,
            json!({"path": "lib.rs"}),
            jail,
            dir.join("bin").as_os_str(),
        );
        if matches!(jail, Jail::Landlock)
            && ran["said"]
                .as_str()
                .is_some_and(|said| said.contains("Landlock"))
        {
            excused("Landlock");
            continue;
        }
        assert_eq!(ran["said"], notice, "{jail:?}: {ran}");
        assert_eq!(ran["failed"], true, "{jail:?}: {ran}");
        assert_eq!(
            ran["brief"],
            notice.lines().next().unwrap_or_default(),
            "{jail:?}"
        );
    }
}

/// What `casper grammar ARGS` answers, with `env` set.
fn grammar(args: &[&str], env: &[(&str, &OsStr)]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_casper"));
    command.arg("grammar").args(args).env_remove("CASPER_JAIL");
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("casper");
    serde_json::from_slice(&out.stdout).expect("a reply")
}

#[test]
fn fetching_without_git_or_building_without_a_compiler_names_what_is_missing() {
    let dir = Scratch::new("grammar", "tools");
    let at = dir.display().to_string();
    let reply = grammar(
        &["fetch", "--dir", &at, "rust"],
        &[("PATH", dir.join("bin").as_os_str())],
    );
    assert_eq!(reply["ok"], false, "{reply}");
    let said = reply["error"].as_str().unwrap_or_default();
    assert!(
        said.contains("git is not installed or not on PATH"),
        "{said}"
    );
    let head = dir.join("src/tree-sitter/.git");
    std::fs::create_dir_all(&head).expect("a checkout");
    let rev = &casper::grammars::lock().runtime.rev;
    std::fs::write(head.join("HEAD"), format!("{rev}\n")).expect("its head");
    let reply = grammar(
        &["build", "--dir", &at, "rust"],
        &[("CC", OsStr::new("casper-no-such-cc"))],
    );
    let said = reply["error"].as_str().unwrap_or_default();
    assert!(
        said.contains("casper-no-such-cc is not installed or not on PATH"),
        "{said}"
    );
}
