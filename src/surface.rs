//! Holding rows a tool asked for, one frame at a time.
//!
//! ```text
//! casper surface <tool>     frames on stdin, frames on stdout, until it is done
//! ```
//!
//! Unlike a `run`, the process lives for as long as it holds the rows: one line of JSON per
//! frame, both directions. The harness owns the rows — it says how many in [`ToSurface::Open`],
//! and it clips what comes back.

use crate::lua::engine::Engine;
use crate::tools::{FromSurface, ToSurface};

/// The same loop, for a tenant that is a program on a pty rather than a drawing in Lua.
mod screening;

/// Asking the harness something, from inside the frame a tenant is drawing.
mod asking;
mod knowing;

pub(crate) use asking::{frames, wonder};

/// What a surface is given that a `run` is not.
///
/// `casper.knows` goes on the table here rather than in the VM: only a surface has a harness on
/// the other end of the pipe to ask.
fn lent(engine: &mut Engine) {
    asking::holding();
    engine.lend("knows", knowing::table);
}

/// Run the frame loop for `tool` until it finishes or stdin closes.
///
/// The first frame carries the size and the arguments the call was made with, so nothing opens
/// before it arrives.
pub fn hold(tool: &str, engine: &mut Engine) -> bool {
    lent(engine);
    let mut lines = frames();
    let Some(Ok(first)) = lines.next() else {
        return delivered();
    };
    let ToSurface::Open {
        rows,
        cols,
        holds,
        args,
    } = read(&first)
    else {
        return delivered();
    };
    // `holds` says whether this terminal ever reports a key coming back up; a tenant told
    // otherwise waits for a release that never comes.
    let size = serde_json::json!({"rows": rows, "cols": cols, "holds": holds});
    // A `screen` declaration is asked about before `surface`, and takes precedence over it.
    if let Some(spec) = engine
        .screen(tool, &args, &size)
        .as_ref()
        .and_then(crate::pty::Spec::from_json)
    {
        screening::hold(&spec, rows, cols, lines);
        return delivered();
    }
    if !engine.open(tool, &args, &size) {
        // No `surface` was declared. Said rather than silent: the harness is holding rows.
        say(&FromSurface::Done {
            answered: String::new(),
        });
        return delivered();
    }
    // Drawn once before any input, so the rows are filled the moment they appear.
    let mut opened = size.clone();
    opened["kind"] = serde_json::Value::String("open".to_owned());
    if !offer(engine, &opened) {
        return delivered();
    }

    for line in lines {
        let Ok(line) = line else {
            return delivered();
        };
        let event = match read(&line) {
            ToSurface::Key { key, state } => serde_json::json!({
                "kind": "key",
                "key": key,
                "state": state,
            }),
            // Already in this surface's own coordinates: row 0 is its first row, and nothing
            // outside the rows it was granted ever arrives.
            ToSurface::Mouse {
                kind,
                button,
                row,
                col,
            } => serde_json::json!({
                "kind": "mouse",
                "what": kind,
                "button": button,
                "row": row,
                "col": col,
            }),
            ToSurface::Tick => serde_json::json!({"kind": "tick"}),
            ToSurface::Resize { rows, cols, holds } => {
                serde_json::json!({"kind": "resize", "rows": rows, "cols": cols, "holds": holds})
            }
            // The tenant is told rather than killed, so one holding something can put it down.
            ToSurface::Close => {
                let _ = engine.frame(&serde_json::json!({"kind": "close"}));
                return delivered();
            }
            // A second open, or an answer nobody is waiting on any more.
            ToSurface::Open { .. } | ToSurface::Answer { .. } => continue,
        };
        if !offer(engine, &event) {
            return delivered();
        }
    }
    delivered()
}

/// Hand one frame to the tenant and say what it drew. `false` when the surface is over.
fn offer(engine: &mut Engine, event: &serde_json::Value) -> bool {
    let Some(drew) = engine.frame(event) else {
        // It raised, or nothing is open; either way the rows can never be filled again.
        say(&FromSurface::Done {
            answered: String::new(),
        });
        return false;
    };
    // An answer ends it, and is checked before the lines: a frame carrying both is finished.
    if let Some(answered) = drew.get("answered").and_then(serde_json::Value::as_str) {
        say(&FromSurface::Done {
            answered: answered.to_owned(),
        });
        return false;
    }
    let lines = drew
        .get("lines")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    // Optional, and a malformed one is no cursor rather than a dropped frame.
    let cursor = drew
        .get("cursor")
        .cloned()
        .and_then(|at| serde_json::from_value(at).ok());
    match serde_json::from_value(lines) {
        Ok(lines) => say(&FromSurface::Draw { lines, cursor }),
        // Nothing readable to draw is not fatal: the rows keep what they had and the loop goes on.
        Err(_) => true,
    }
}

/// One frame in, falling back to a tick for anything unreadable, so that a frame from a newer
/// harness does not end the surface.
fn read(line: &str) -> ToSurface {
    serde_json::from_str(line).unwrap_or(ToSurface::Tick)
}

/// The surface is over before it opened, said as a frame rather than as silence: the harness has
/// already reserved the rows by the time this process starts.
pub fn nothing_to_draw() -> bool {
    say(&FromSurface::Done {
        answered: String::new(),
    })
}

/// One frame out. `false` when it did not reach the harness.
///
/// Written rather than printed: `println!` panics on a failed write, so a harness that went away
/// would panic this process instead of returning `false`. Flushed every frame, or a buffered
/// tenant's rows arrive in batches.
fn say(frame: &FromSurface) -> bool {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(frame) else {
        crate::noted!("surface: a frame would not encode");
        return false;
    };
    let mut out = std::io::stdout().lock();
    match out
        .write_all(line.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush())
    {
        Ok(()) => true,
        Err(why) => {
            crate::noted!("surface: a frame did not reach the harness: {why}");
            DELIVERED.store(false, std::sync::atomic::Ordering::Relaxed);
            false
        }
    }
}

/// Whether every frame this process wrote reached the harness. [`say`] is the only writer.
static DELIVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// What [`DELIVERED`] holds, as the verdict every way out of [`hold`] returns.
fn delivered() -> bool {
    DELIVERED.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_frame_is_a_tick_rather_than_the_end() {
        assert_eq!(read(r#"{"event":"nothing_yet"}"#), ToSurface::Tick);
        assert_eq!(read("not json at all"), ToSurface::Tick);
    }

    #[test]
    fn the_frames_this_build_knows_read_back_as_themselves() {
        assert_eq!(read(r#"{"event":"tick"}"#), ToSurface::Tick);
        assert_eq!(read(r#"{"event":"close"}"#), ToSurface::Close);
        // No `state` on the wire is a terminal from before the Kitty protocol.
        assert_eq!(
            read(r#"{"event":"key","key":"space"}"#),
            ToSurface::Key {
                key: "space".to_owned(),
                state: crate::tools::Held::Down,
            }
        );
        assert_eq!(
            read(r#"{"event":"key","key":"space","state":"up"}"#),
            ToSurface::Key {
                key: "space".to_owned(),
                state: crate::tools::Held::Up,
            }
        );
    }

    #[test]
    fn a_click_arrives_in_this_surface_own_rows() {
        // Already translated by the harness: row zero is this surface's first row.
        assert_eq!(
            read(r#"{"event":"mouse","kind":"press","button":"left","row":2,"col":11}"#),
            ToSurface::Mouse {
                kind: crate::tools::Pointed::Press,
                button: Some(crate::tools::Button::Left),
                row: 2,
                col: 11,
            }
        );
    }
}

/// The frame loop, driven the way the harness drives it.
#[cfg(test)]
mod holding {
    use crate::lua::engine::Engine;

    /// `casper.knows` exists inside a surface and nowhere else.
    #[test]
    fn a_surface_is_lent_the_question_a_run_is_not() {
        // Asserted in Lua: the `casper` table can be seen from nowhere else.
        let mut engine = Engine::new();
        assert!(
            engine
                .run(
                    r#"assert(casper.knows == nil, "a run has it")"#,
                    "before.lua"
                )
                .is_ok(),
            "a `run` cannot ask the harness anything: there is no harness on the pipe"
        );

        super::lent(&mut engine);
        assert!(
            engine
                .run(
                    r#"assert(type(casper.knows) == "function", "a surface has not")"#,
                    "after.lua"
                )
                .is_ok(),
            "`hold` lends it, and this is the line that says so"
        );
    }

    /// An engine with one surface tool declared, and the frames it draws for `events`.
    fn played(source: &str, events: &[serde_json::Value]) -> Vec<serde_json::Value> {
        let mut engine = Engine::new();
        engine.run(source, "tools.lua").expect("it loads");
        assert!(
            engine.open(
                "t",
                &serde_json::json!({}),
                &serde_json::json!({"rows": 4, "cols": 20})
            ),
            "the surface opened"
        );
        events
            .iter()
            .map(|event| engine.frame(event).unwrap_or(serde_json::Value::Null))
            .collect()
    }

    const COUNTER: &str = r#"
        casper.tool("t", { description = "d", parameters = {},
          run = function() return casper.surface{ rows = 4, about = "a counter" } end,
          surface = function(args, size)
            local n = 0
            return function(event)
              if event.kind == "key" and event.key == "q" then return { answered = "quit" } end
              n = n + 1
              return { lines = { { { role = "text", text = tostring(n) .. "/" .. size.rows } } } }
            end
          end })
    "#;

    #[test]
    fn a_surface_keeps_its_state_between_frames() {
        // The tenant returns a closure, so `n` lives in its upvalues.
        let drew = played(
            COUNTER,
            &[
                serde_json::json!({"kind": "tick"}),
                serde_json::json!({"kind": "tick"}),
                serde_json::json!({"kind": "tick"}),
            ],
        );
        let said = |frame: &serde_json::Value| {
            frame["lines"][0][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        };
        assert_eq!(said(&drew[0]), "1/4");
        assert_eq!(said(&drew[2]), "3/4");
    }

    #[test]
    fn the_size_it_was_given_reaches_the_tenant() {
        let drew = played(COUNTER, &[serde_json::json!({"kind": "tick"})]);
        assert!(
            drew[0]["lines"][0][0]["text"]
                .as_str()
                .unwrap_or_default()
                .ends_with("/4")
        );
    }

    #[test]
    fn answering_ends_it_and_says_what_was_chosen() {
        let drew = played(COUNTER, &[serde_json::json!({"kind": "key", "key": "q"})]);
        assert_eq!(drew[0]["answered"], "quit");
    }

    #[test]
    fn a_tenant_may_ask_for_the_terminal_own_caret() {
        // The terminal's own caret, which an IME and a screen reader follow.
        let mut engine = Engine::new();
        engine
            .run(
                r#"casper.tool("t", { description = "d", parameters = {},
                     run = function() return casper.surface{ rows = 2, about = "x" } end,
                     surface = function() return function()
                       return { lines = { { { role = "text", text = "name: " } } },
                                cursor = { row = 0, col = 6 } }
                     end end })"#,
                "tools.lua",
            )
            .expect("it loads");
        assert!(engine.open(
            "t",
            &serde_json::json!({}),
            &serde_json::json!({"rows": 2, "cols": 20})
        ));
        let drew = engine
            .frame(&serde_json::json!({"kind": "tick"}))
            .expect("it drew");
        assert_eq!(drew["cursor"]["col"], 6);
    }

    #[test]
    fn a_tenant_that_raises_ends_rather_than_looping() {
        let mut engine = Engine::new();
        engine
            .run(
                r#"casper.tool("t", { description = "d", parameters = {},
                     run = function() return casper.surface{ rows = 2, about = "x" } end,
                     surface = function() return function() error("no") end end })"#,
                "tools.lua",
            )
            .expect("it loads");
        assert!(engine.open(
            "t",
            &serde_json::json!({}),
            &serde_json::json!({"rows": 2, "cols": 20})
        ));
        assert!(engine.frame(&serde_json::json!({"kind": "tick"})).is_none());
    }

    #[test]
    fn an_ordinary_tool_has_no_surface_to_open() {
        let mut engine = Engine::new();
        engine
            .run(
                r#"casper.tool("t", { description = "d", parameters = {},
                     run = function() return "done" end })"#,
                "tools.lua",
            )
            .expect("it loads");
        assert!(!engine.open(
            "t",
            &serde_json::json!({}),
            &serde_json::json!({"rows": 4, "cols": 20})
        ));
    }
}
