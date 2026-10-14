//! The tools casper ships, by name, against the list in `tests/roster.txt`.
//!
//! A tool added to or removed from `config/tools.lua` fails this test until the list is edited.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use std::process::Command;

/// The names `casper tools` lists for this checkout's declarations, sorted.
fn listed() -> Vec<String> {
    let dir = casper::scratch::Scratch::new("casper-roster", "config");
    let into = dir.join("casper");
    std::fs::create_dir_all(&into).expect("mkdir");
    std::fs::write(into.join("tools.lua"), include_str!("../config/tools.lua")).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_casper"))
        .args(["tools", "--json"])
        .env("CASPER_CONFIGURE", "")
        .env("XDG_CONFIG_HOME", &*dir)
        .env("XDG_RUNTIME_DIR", &*dir)
        .env("XDG_DATA_HOME", &*dir)
        .output()
        .expect("casper runs");
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a JSON reply");
    let mut names: Vec<String> = reply["result"]
        .as_array()
        .expect("a list of cards")
        .iter()
        .filter_map(|card| card["name"].as_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

#[test]
fn the_shipped_tools_are_exactly_the_roster() {
    let mut roster: Vec<String> = include_str!("roster.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    roster.sort();
    assert_eq!(
        listed(),
        roster,
        "config/tools.lua and tests/roster.txt disagree"
    );
}
