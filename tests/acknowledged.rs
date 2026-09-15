//! Fetched declarations run once somebody has said they may, and stop the moment they change.
//!
//! Against the real binary, end to end: the unit tests under `src/acknowledged.rs` and
//! `src/plugins.rs` reach the digest arithmetic and the load order but not the arrangement of the
//! two. A package's tool is asked for by name rather than counted, because the shipped
//! declarations bring thirteen of their own.

use casper::scratch::Scratch;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const CASPER: &str = env!("CARGO_BIN_EXE_casper");

/// The tool the fetched package declares, named so nothing shipped can be mistaken for it.
const FROM_THE_PACKAGE: &str = "acknowledge-probe";

/// The tool the owner's own `plugin/` directory declares.
const FROM_THE_OWNER: &str = "owner-probe";

/// One declaration file, declaring one tool that does nothing.
fn declaring(tool: &str) -> String {
    format!(
        "casper.tool({tool:?}, {{\n  \
           description = \"a probe\",\n  \
           parameters = {{ type = \"object\" }},\n  \
           run = function() return {{ said = {tool:?} }} end,\n\
         }})\n"
    )
}

/// A machine with the shipped declarations, a package under `site/`, and a file of the owner's.
///
/// The two probes are the whole point: one arrived by being fetched and one was put there by the
/// person, and only the first has to be acknowledged.
fn machine(name: &str) -> Scratch {
    let dir = Scratch::new("casper-acknowledged", name);
    let config = dir.join("config/casper");
    std::fs::create_dir_all(config.join("plugin")).expect("mkdir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("config/tools.lua"),
        config.join("tools.lua"),
    )
    .expect("the shipped declarations");
    std::fs::write(config.join("plugin/owner.lua"), declaring(FROM_THE_OWNER)).expect("wrote");

    std::fs::create_dir_all(package(&dir).parent().expect("a plugin directory")).expect("mkdir");
    std::fs::write(package(&dir), declaring(FROM_THE_PACKAGE)).expect("wrote");
    dir
}

/// The one file inside the installed package.
fn package(dir: &Scratch) -> std::path::PathBuf {
    dir.join("data/casper/site/pack/probes/start/one/plugin/probe.lua")
}

fn asked(dir: &Scratch, verb: &str) -> Output {
    Command::new(CASPER)
        .arg(verb)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_RUNTIME_DIR", dir.join("runtime"))
        .stdin(Stdio::null())
        .output()
        .expect("casper runs")
}

fn offers(dir: &Scratch, tool: &str) -> bool {
    let out = asked(dir, "tools");
    assert!(out.status.success(), "casper answered");
    let listed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the reply is the family's shape");
    listed["result"]
        .as_array()
        .expect("rows")
        .iter()
        .any(|card| card["name"] == tool)
}

fn aside(dir: &Scratch, verb: &str) -> String {
    String::from_utf8_lossy(&asked(dir, verb).stderr).into_owned()
}

#[test]
fn a_fetched_package_does_not_run_until_it_has_been_acknowledged() {
    let dir = machine("held");
    assert!(
        !offers(&dir, FROM_THE_PACKAGE),
        "a package that arrived by being fetched ran on sight"
    );
    let said = aside(&dir, "tools");
    assert!(
        said.contains("casper acknowledge") && said.contains("probe.lua"),
        "and nothing said why: {said:?}"
    );
}

#[test]
fn the_owners_own_file_runs_on_sight() {
    // The other half of the distinction, and what stops the test above from passing against a
    // casper that simply never loads a `plugin/` directory at all.
    let dir = machine("owner");
    assert!(
        offers(&dir, FROM_THE_OWNER),
        "asking somebody to confirm their own configuration is a prompt nobody reads"
    );
}

#[test]
fn acknowledging_lets_it_run() {
    let dir = machine("cleared");
    assert!(!offers(&dir, FROM_THE_PACKAGE), "held to begin with");
    let out = asked(&dir, "acknowledge");
    assert!(out.status.success(), "the manifest was written");
    assert!(
        offers(&dir, FROM_THE_PACKAGE),
        "acknowledged and still not running"
    );
}

#[test]
fn a_package_that_changed_is_held_again() {
    // The reason a manifest records a digest rather than a name: a package can change under you
    // between one run and the next, and acknowledging is about the bytes that were read.
    let dir = machine("changed");
    assert!(asked(&dir, "acknowledge").status.success());
    assert!(offers(&dir, FROM_THE_PACKAGE), "cleared");

    let changed = declaring(FROM_THE_PACKAGE) + "-- and something else\n";
    std::fs::write(package(&dir), changed).expect("wrote");
    assert!(
        !offers(&dir, FROM_THE_PACKAGE),
        "a package that changed after being acknowledged went on running"
    );
    let said = aside(&dir, "tools");
    assert!(
        said.contains("casper acknowledge"),
        "and nothing said why: {said:?}"
    );
}
