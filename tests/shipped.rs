//! The declarations casper actually ships, driven the way the harness drives them.
//!
//! Everything else tests a tenant written for the test. These load `config/tools.lua` — the same
//! text `main.rs` embeds — because the bugs that reach a person are in the thing that ships, and a
//! toy tenant written beside the assertion agrees with it by construction.

use casper::lua::engine::Engine;

/// The shipped declarations, with `name`'s surface open at `rows` by `cols`.
fn opened(name: &str, args: &serde_json::Value, rows: u16, cols: u16) -> Engine {
    let mut engine = Engine::new();
    engine
        .run(include_str!("../config/tools.lua"), "tools.lua")
        .expect("the shipped declarations load");
    assert!(
        engine.open(name, args, &serde_json::json!({"rows": rows, "cols": cols, "holds": true})),
        "{name} declared no surface"
    );
    engine
}

/// One key, in one state.
fn key(engine: &mut Engine, name: &str, state: &str) -> serde_json::Value {
    engine
        .frame(&serde_json::json!({"kind": "key", "key": name, "state": state}))
        .unwrap_or(serde_json::Value::Null)
}

/// Which row a permission prompt is pointing at, by its label.
fn pointing(drew: &serde_json::Value) -> String {
    drew["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let text: String = row
                .as_array()?
                .iter()
                .filter_map(|span| span["text"].as_str())
                .collect();
            text.trim_start().strip_prefix("> ").map(str::to_owned)
        })
        .next()
        .unwrap_or_default()
}

fn a_question() -> serde_json::Value {
    serde_json::json!({
        "tool": "read", "verb": "read", "subject": "/etc/hosts",
        "offers": [
            {"id": "0", "label": "Just this once"},
            {"id": "1", "label": "Anything under /etc"},
            {"id": "2", "label": "Anything at all"},
            {"id": "no", "label": "Deny"},
        ],
    })
}

#[test]
fn one_press_of_an_arrow_moves_the_permission_prompt_one_row() {
    // **The release is not a second press.** Where the Kitty protocol is live every keystroke
    // arrives twice, and a list that acted on both moved two rows for one press — which is a
    // person selecting the wrong permission and not knowing why.
    let mut engine = opened("permission", &a_question(), 9, 60);
    let down = key(&mut engine, "down", "down");
    assert_eq!(pointing(&down), "Anything under /etc");
    let up = key(&mut engine, "down", "up");
    assert_eq!(pointing(&up), "Anything under /etc", "the release moved it");
}

#[test]
fn holding_an_arrow_still_scrolls_the_list() {
    // The other half, and why the fix is not "ignore everything but a press": a repeat says the
    // key is still down, and a list that dropped those would need a tap per row.
    let mut engine = opened("permission", &a_question(), 9, 60);
    key(&mut engine, "down", "down");
    let held = key(&mut engine, "down", "repeat");
    assert_eq!(pointing(&held), "Anything at all");
}

#[test]
fn a_terminal_that_says_nothing_about_holding_still_moves_one_row() {
    // Every terminal without the protocol, where a key is one bare press and there is no state on
    // the frame at all. The guard must not have turned those into nothing.
    let mut engine = opened("permission", &a_question(), 9, 60);
    let drew = engine
        .frame(&serde_json::json!({"kind": "key", "key": "down"}))
        .expect("it drew");
    assert_eq!(pointing(&drew), "Anything under /etc");
}

#[test]
fn enter_answers_with_the_row_it_is_pointing_at() {
    // And not twice. An answer is the end of the surface, so a release arriving behind it must
    // not be read as a second choice.
    let mut engine = opened("permission", &a_question(), 9, 60);
    key(&mut engine, "down", "down");
    key(&mut engine, "down", "up");
    let chosen = key(&mut engine, "enter", "down");
    assert_eq!(chosen["answered"], "1");
}

#[test]
fn escape_denies_rather_than_choosing_whatever_is_under_the_cursor() {
    let mut engine = opened("permission", &a_question(), 9, 60);
    key(&mut engine, "down", "down");
    assert_eq!(key(&mut engine, "esc", "down")["answered"], "no");
}
