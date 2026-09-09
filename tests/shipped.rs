//! The declarations casper actually ships, driven the way the harness drives them.
//!
//! These load `config/tools.lua`, the same text `main.rs` embeds, rather than a tenant written
//! beside the assertion.

use casper::lua::engine::Engine;

/// The shipped declarations, with `name`'s surface open at `rows` by `cols`.
fn opened(name: &str, args: &serde_json::Value, rows: u16, cols: u16) -> Engine {
    let mut engine = Engine::new();
    engine
        .run(include_str!("../config/tools.lua"), "tools.lua")
        .expect("the shipped declarations load");
    assert!(
        engine.open(
            name,
            args,
            &serde_json::json!({"rows": rows, "cols": cols, "holds": true})
        ),
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
    // Where the Kitty protocol is live every keystroke arrives twice, so a list that acts on the
    // release as well as the press moves two rows for one press.
    let mut engine = opened("permission", &a_question(), 9, 60);
    let down = key(&mut engine, "down", "down");
    assert_eq!(pointing(&down), "Anything under /etc");
    let up = key(&mut engine, "down", "up");
    assert_eq!(pointing(&up), "Anything under /etc", "the release moved it");
}

#[test]
fn holding_an_arrow_still_scrolls_the_list() {
    // A repeat says the key is still down; a list that dropped those would need a tap per row.
    let mut engine = opened("permission", &a_question(), 9, 60);
    key(&mut engine, "down", "down");
    let held = key(&mut engine, "down", "repeat");
    assert_eq!(pointing(&held), "Anything at all");
}

#[test]
fn a_terminal_that_says_nothing_about_holding_still_moves_one_row() {
    // Every terminal without the protocol sends one bare press with no state on the frame.
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

/// The games read the same keyboard two ways, and both readings have to keep working.
mod games {
    use super::{key, opened};

    /// Every row of a frame, joined, so a readout can be searched for.
    fn text(drew: &serde_json::Value) -> String {
        drew["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                Some(
                    row.as_array()?
                        .iter()
                        .filter_map(|span| span["text"].as_str())
                        .collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_release_of_the_quit_key_does_not_end_a_game() {
        for game in ["dino", "birdy"] {
            let mut engine = opened(game, &serde_json::json!({}), 8, 60);
            let after = key(&mut engine, "q", "up");
            assert!(after.get("answered").is_none(), "{game} quit on a release");
            let after = key(&mut engine, "esc", "up");
            assert!(after.get("answered").is_none(), "{game} quit on a release");
        }
    }

    #[test]
    fn a_press_of_the_quit_key_still_ends_it_and_reports_the_score() {
        for game in ["dino", "birdy"] {
            let mut engine = opened(game, &serde_json::json!({}), 8, 60);
            let done = key(&mut engine, "Q", "down");
            assert_eq!(
                done["answered"]
                    .as_str()
                    .unwrap_or_default()
                    .split(' ')
                    .next(),
                Some("scored"),
                "{game}: {done}"
            );
        }
    }

    #[test]
    fn a_game_still_sees_a_key_coming_back_up() {
        // The half `casper.tapped` deliberately hides, and the reason the games do not use it for
        // the jump: a release is what ends one, and a game that stopped seeing them would have
        // every jump the same height.
        let mut engine = opened("dino", &serde_json::json!({}), 8, 60);
        key(&mut engine, "space", "down");
        let after = key(&mut engine, "space", "up");
        assert!(
            text(&after).contains("space up"),
            "the readout lost the release: {}",
            text(&after)
        );
    }
}

/// What the emulator behind a `screen` tool cannot read, against real programs. A gap here is a
/// screen that renders subtly wrong with nothing on it to say why.
mod conformance {
    use casper::pty::{Screen, Spec};
    use std::time::{Duration, Instant};

    /// Run `command` for long enough to draw, and report what the emulator threw away.
    fn dropped_by(command: &str, rows: u16, cols: u16) -> Vec<(String, usize)> {
        // A home of this test's own: `btop` writes `$XDG_CONFIG_HOME/btop` and `top` writes
        // `$XDG_CONFIG_HOME/procps` on the way out, neither of which `gate-hermetic` watches.
        let home = casper::scratch::Scratch::new("casper-conformance", "home");
        let at = |name: &str| home.join(name).display().to_string();
        let spec = Spec {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), command.to_owned()],
            env: vec![
                ("HOME".to_owned(), home.display().to_string()),
                ("XDG_CONFIG_HOME".to_owned(), at("config")),
                ("XDG_DATA_HOME".to_owned(), at("data")),
                ("XDG_STATE_HOME".to_owned(), at("state")),
                ("XDG_CACHE_HOME".to_owned(), at("cache")),
            ],
            ..Spec::default()
        };
        let Ok(mut screen) = Screen::open(&spec, rows, cols) else {
            return Vec::new();
        };
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if !screen.read() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        screen.dropped()
    }

    /// Whether a program is on this machine, so a missing one skips rather than fails.
    fn here(program: &str) -> bool {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {program}")])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// Sequences the emulator drops that change nothing about what is drawn, each with the reason
    /// beside it. Anything not on this list is a gap nobody has looked at yet.
    const HARMLESS: &[(&str, &str)] = &[
        (
            "CSI ?2026h",
            "synchronized output: a hint to hold the repaint until the frame is whole",
        ),
        (
            "CSI ?2026l",
            "and the end of one. Nothing here repaints mid-frame anyway",
        ),
        (
            "CSI ?1015h",
            "urxvt mouse coordinates, asked for beside the SGR ones casper reads",
        ),
        (
            "CSI t",
            "window manipulation. `less` sends `CSI 22;0;0t`, pushing the window title \
                   onto a stack it pops on the way out. There is no window and no title here",
        ),
    ];

    #[test]
    fn the_programs_on_this_machine_are_understood_in_full() {
        let programs = [
            ("top -b -n 2", "top"),
            ("btop", "btop"),
            ("seq 1 200 | less", "less"),
            ("printf 'hello\\n'; ls --color=always /", "ls"),
        ];
        for (command, program) in programs {
            if !here(program) {
                continue;
            }
            let unexplained: Vec<(String, usize)> = dropped_by(command, 24, 90)
                .into_iter()
                .filter(|(what, _)| !HARMLESS.iter().any(|(known, _)| known == what))
                .collect();
            assert!(
                unexplained.is_empty(),
                "{program} uses sequences nobody has looked at: {unexplained:?}"
            );
        }
    }

    #[test]
    fn the_canary_is_awake() {
        // A backward tab: the one sequence the emulator does not know and the rewriter leaves
        // alone, so seeing it reported is what says the wiring works.
        let dropped = dropped_by(r"printf '\033[4Z'; sleep 1", 5, 20);
        assert_eq!(
            dropped,
            [("CSI Z".to_owned(), 1)],
            "the canary said nothing"
        );
    }
}
