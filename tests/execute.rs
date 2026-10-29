//! `execute` through the binary, the way a harness calls it: a script that reads and lists, the
//! limits it runs under, the tools it may not reach, and the jail its inner calls stay in.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::scratch::Scratch;
use casper::tools::Ran;
use serde_json::json;
use std::io::Write as _;

#[path = "support/native_namespace.rs"]
mod native_namespace;

/// A tool slower than a script's whole time budget, declared callable the way a plugin would.
const SLOW: &str = r#"casper.tool("slow", {
  description = "Wait a while, then say so.",
  parameters = { type = "object", properties = {} },
  needs = "read",
  script = true,
  run = function() casper.exec("sleep", { "0.7" }); return "slept" end,
})
casper.tool("sneaky", {
  description = "A writer that says it is callable.",
  parameters = { type = "object", properties = {} },
  needs = "write",
  script = true,
  run = function() return "wrote" end,
})
"#;

fn fixture(name: &str) -> Scratch {
    let dir = Scratch::new("casper-execute", name);
    for path in ["config/casper/plugin", "work/sub", "home/.ssh"] {
        std::fs::create_dir_all(dir.join(path)).expect("fixture directory");
    }
    std::fs::write(
        dir.join("config/casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("tools");
    std::fs::write(dir.join("config/casper/plugin/slow.lua"), SLOW).expect("plugin");
    std::fs::write(dir.join("work/a.txt"), "alpha\nbeta\n").expect("a.txt");
    std::fs::write(dir.join("home/.ssh/id"), "SYNTHETIC_SECRET\n").expect("secret");
    dir
}

fn call(dir: &Scratch, tool: &str, args: serde_json::Value, jail: bool) -> Ran {
    let namespace = native_namespace::NativeNamespace::new(dir, &dir.join("work"));
    let binary = std::env::var_os("CASPER_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_casper").into());
    let mut command = namespace.command(binary);
    command
        .arg("run")
        .current_dir(dir.join("work"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_RUNTIME_DIR", &**dir)
        .env("HOME", dir.join("home"))
        .env_remove("CASPER_CONFIGURE")
        .env_remove("CASPER_JAIL")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    if jail {
        command.env("CASPER_JAIL", "1");
    }
    let mut child = command.spawn().expect("casper");
    let body = json!({ "tool": tool, "args": args }).to_string();
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(body.as_bytes())
        .expect("call");
    let out = child.wait_with_output().expect("output");
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply");
    assert_eq!(reply["ok"], true, "{reply}");
    serde_json::from_value(reply["result"][0].clone()).expect("Ran")
}

fn script(dir: &Scratch, code: &str) -> Ran {
    call(dir, "execute", json!({ "code": code }), true)
}

#[test]
fn a_script_reads_and_lists_and_prints_what_it_found() {
    let dir = fixture("reads");
    let code = "text = await read(path='a.txt')\nlisting = await ls(path='.')\n\
                print(text.splitlines()[1])\nprint('sub/' in listing)";
    let ran = script(&dir, code);
    assert!(!ran.failed, "{}", ran.said);
    assert_eq!(ran.said, "beta\nTrue");
    assert!(
        ran.brief
            .unwrap_or_default()
            .starts_with("execute: 4 lines of Python")
    );
}

#[test]
fn gather_keeps_the_others_when_one_call_fails() {
    let dir = fixture("gather");
    let code = "a, b, c = await gather(read(path='a.txt'), read(path='missing.txt'), tree(path='.'))\n\
                print(a.splitlines()[0])\nprint(b.startswith('[ERROR] read:'))\nprint('a.txt' in c)";
    let ran = script(&dir, code);
    assert_eq!(ran.said, "alpha\nTrue\nTrue", "{ran:?}");
}

#[test]
fn a_tool_that_writes_runs_or_reaches_is_refused_whatever_it_declares() {
    let dir = fixture("refused");
    for (code, verb) in [
        ("await write(path='made.txt', contents='x')", "write"),
        (
            "await edit(path='a.txt', old='alpha', new='omega')",
            "write",
        ),
        ("await shell(command='touch made.txt')", "run"),
        (
            "await web(action='fetch', url='https://example.com')",
            "reach",
        ),
        ("await sneaky()", "write"),
    ] {
        let ran = script(&dir, code);
        assert!(ran.failed, "{code}: {}", ran.said);
        assert!(
            ran.said.contains("PermissionError") && ran.said.contains(&format!("needs \"{verb}\"")),
            "{code}: {}",
            ran.said
        );
    }
    assert!(!dir.join("work/made.txt").exists());
    let a = std::fs::read_to_string(dir.join("work/a.txt")).expect("a.txt");
    assert_eq!(a, "alpha\nbeta\n");
}

#[test]
fn an_import_outside_the_interpreters_own_is_refused() {
    let dir = fixture("import");
    for module in ["subprocess", "socket", "urllib"] {
        let ran = script(&dir, &format!("import {module}"));
        assert!(ran.failed);
        assert!(
            ran.said.contains("ModuleNotFoundError"),
            "{module}: {}",
            ran.said
        );
    }
}

#[test]
fn a_script_past_its_time_is_stopped() {
    let dir = fixture("timeout");
    let began = std::time::Instant::now();
    let ran = call(
        &dir,
        "execute",
        json!({ "code": "while True:\n    pass", "timeout": 1 }),
        false,
    );
    assert!(began.elapsed() < std::time::Duration::from_secs(20));
    assert!(ran.failed && ran.keep, "{}", ran.said);
    assert_eq!(ran.said, "[timed out after 1s; no output before the cut]");
}

#[test]
fn waiting_on_tools_does_not_spend_the_scripts_time() {
    let dir = fixture("waiting");
    let code = "a = await slow()\nb = await slow()\nprint(a + b)";
    let ran = call(
        &dir,
        "execute",
        json!({ "code": code, "timeout": 1 }),
        false,
    );
    assert!(!ran.failed, "{}", ran.said);
    assert_eq!(ran.said, "sleptslept");
}

#[test]
fn a_script_past_its_memory_is_stopped() {
    let dir = fixture("memory");
    let ran = script(&dir, "x = [0] * 10_000_000");
    assert!(ran.failed);
    assert!(
        ran.said.contains("MemoryError: memory limit exceeded"),
        "{}",
        ran.said
    );
}

#[test]
fn what_a_script_prints_is_capped_and_the_cut_is_said() {
    let dir = fixture("capped");
    let ran = script(&dir, "for i in range(3000):\n    print(i)");
    let kept: Vec<&str> = ran.said.lines().collect();
    assert_eq!(kept[1999], "1999");
    assert_eq!(kept[2000], "");
    assert!(kept[2001].starts_with("[truncated "), "{}", kept[2001]);
}

#[test]
fn a_jailed_inner_read_of_a_credential_is_refused_as_a_direct_one_is() {
    let dir = fixture("jailed");
    let secret = dir.join("home/.ssh/id").display().to_string();
    let direct = call(&dir, "read", json!({ "path": secret }), true);
    assert!(direct.failed, "{}", direct.said);
    let code = format!(
        "try:\n    await read(path='{secret}')\nexcept RuntimeError as e:\n    print(e)\n\
         try:\n    open('{secret}').read()\nexcept OSError as e:\n    print(e)"
    );
    let inner = call(&dir, "execute", json!({ "code": code }), true);
    assert!(!inner.failed, "{}", inner.said);
    assert!(!inner.said.contains("SYNTHETIC_SECRET"), "{}", inner.said);
    let refused = direct.said.trim();
    assert!(
        inner.said.starts_with(&format!("read: {refused}\n")),
        "{}\n{refused}",
        inner.said
    );
    assert!(inner.said.ends_with(refused), "{}\n{refused}", inner.said);
    let allowed = call(
        &dir,
        "execute",
        json!({ "code": "print(open('a.txt').read())" }),
        true,
    );
    assert_eq!(allowed.said, "alpha\nbeta");
}

#[test]
fn the_card_lists_what_a_script_may_call_and_nothing_else() {
    let dir = fixture("card");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_casper"))
        .args(["tools", "--json"])
        .current_dir(dir.join("work"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_RUNTIME_DIR", &*dir)
        .env_remove("CASPER_CONFIGURE")
        .output()
        .expect("casper tools");
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply");
    let cards = reply["result"].as_array().expect("cards");
    let execute = cards
        .iter()
        .find(|c| c["name"] == "execute")
        .expect("execute");
    assert_eq!(execute["needs"], "read");
    let said = execute["description"].as_str().unwrap_or_default();
    for listed in [
        "- read(path: str, limit: int = None, offset: int = None) -> str",
        "- ls(",
        "- tree(",
        "- slow() -> str",
    ] {
        assert!(said.contains(listed), "{listed} missing from {said}");
    }
    for absent in [
        "- write(",
        "- shell(",
        "- sese(",
        "- web(",
        "- sneaky(",
        "- execute(",
    ] {
        assert!(!said.contains(absent), "{absent} in {said}");
    }
    assert!(
        cards.iter().all(|c| c.get("script").is_none()),
        "script crossed the wire"
    );
}
