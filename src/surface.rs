//! Holding rows a tool asked for, one frame at a time.
//!
//! ```text
//! casper surface <tool>     frames on stdin, frames on stdout, until it is done
//! ```
//!
//! **Why this is not `run`.** A call is one exec: request on stdin, reply on stdout, exit. A
//! surface redraws whenever a key arrives or time passes, so the process lives for as long as it
//! holds the rows and frames cross both ways. One exec per keypress would answer a picker and
//! could not animate anything.
//!
//! One line of JSON per frame, both directions — a length prefix buys nothing over a pipe where
//! the frames are small and a newline cannot appear inside one.
//!
//! **The harness owns the rows.** It says how many in [`ToSurface::Open`], and it clips what comes
//! back. A tenant drawing more than it was given would run over whatever is below it, and only
//! the harness knows what that is.

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
/// From here there is a harness on the other end of the pipe, so a tenant may ask it things —
/// and only from here, which is why `casper.knows` goes on the table at this point rather than
/// in the VM. A `run` is one exec whose stdout is its reply; a question written there would
/// reach the harness as the tool's own result, from a tool that looked like it had failed.
///
/// A named function rather than two lines inside [`hold`], so a test can call the same thing
/// the loop calls. Inline, the only test that could reach it would have to drive a real pipe,
/// which is why the capability could have been dropped in a refactor with the whole suite green.
fn lent(engine: &mut Engine) {
    asking::holding();
    engine.lend("knows", knowing::table);
}

/// Run the frame loop for `tool` until it finishes or stdin closes.
///
/// The arguments the call was made with arrive on the first frame, so a surface opens knowing what
/// it was asked about — a permission needs the command, a picker needs the list.
pub fn hold(tool: &str, engine: &mut Engine) {
    lent(engine);
    let mut lines = frames();
    // The first frame says how much room there is and what the call was given. Opening before it
    // arrives would mean guessing at a size, and a tenant that laid itself out for the wrong one
    // draws once wrongly before it is told.
    let Some(Ok(first)) = lines.next() else {
        return;
    };
    let ToSurface::Open {
        rows,
        cols,
        holds,
        args,
    } = read(&first)
    else {
        // Anything else first is a caller that does not speak this, and answering it would be
        // answering a frame nobody meant to send.
        return;
    };
    // What it was granted, in one table: the rows, the width, and whether this terminal will ever
    // report a key coming back up. A tenant told otherwise waits for a release that never comes,
    // which is how "hold to do more" ends up doing nothing at all on most terminals.
    let size = serde_json::json!({"rows": rows, "cols": cols, "holds": holds});
    // **A program in the rows, if this tool declared one.** Asked before `surface`, because a
    // declaration carrying both is two tenants for one reservation and this is the more specific
    // of the two. From here the loop is a different one — see [`screening`] — and the harness
    // cannot tell which it is talking to.
    if let Some(spec) = engine
        .screen(tool, &args, &size)
        .as_ref()
        .and_then(crate::pty::Spec::from_json)
    {
        screening::hold(&spec, rows, cols, lines);
        return;
    }
    if !engine.open(tool, &args, &size) {
        // No `surface` was declared. Said rather than silent: the harness reserved rows for this
        // and would otherwise hold them for a tenant that is never going to draw.
        say(&FromSurface::Done {
            answered: String::new(),
        });
        return;
    }
    // Drawn once before any input, so the rows are filled the moment they appear rather than on
    // the first keypress.
    let mut opened = size.clone();
    opened["kind"] = serde_json::Value::String("open".to_owned());
    if !offer(engine, &opened) {
        return;
    }

    for line in lines {
        let Ok(line) = line else {
            return;
        };
        let event = match read(&line) {
            ToSurface::Key { key, state } => serde_json::json!({
                "kind": "key",
                "key": key,
                // `down`, `repeat` or `up`. A tenant that only looks at `key` is unaffected.
                "state": state,
            }),
            // Already in this surface's own coordinates: row 0 is its first row. Nothing outside
            // the rows it was granted ever arrives, so a tenant needs no bounds check of its own.
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
            // The reservation is over. The tenant is told rather than killed, so one holding
            // something can put it down.
            ToSurface::Close => {
                let _ = engine.frame(&serde_json::json!({"kind": "close"}));
                return;
            }
            // An answer nobody is waiting on: the tenant asked, gave up, and the harness said so
            // anyway. Nothing to draw about.
            ToSurface::Open { .. } | ToSurface::Answer { .. } => continue,
        };
        if !offer(engine, &event) {
            return;
        }
    }
}

/// Hand one frame to the tenant and say what it drew. `false` when the surface is over.
fn offer(engine: &mut Engine, event: &serde_json::Value) -> bool {
    let Some(drew) = engine.frame(event) else {
        // It raised, or nothing is open. Either way the rows cannot be filled again, and holding
        // them would leave a hole on the screen no key could close.
        say(&FromSurface::Done {
            answered: String::new(),
        });
        return false;
    };
    // An answer ends it. Checked before the lines, so a tenant that draws a farewell *and*
    // answers in the same frame is taken as finished rather than as still drawing.
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
    // Optional, and a malformed one is no cursor rather than a dropped frame: a tenant that got
    // the shape wrong should lose the caret, not the rows it drew around it.
    let cursor = drew
        .get("cursor")
        .cloned()
        .and_then(|at| serde_json::from_value(at).ok());
    match serde_json::from_value(lines) {
        Ok(lines) => {
            say(&FromSurface::Draw { lines, cursor });
            true
        }
        // A frame that drew nothing readable is not fatal on its own — a tenant may answer a tick
        // it has nothing to do with — so the rows keep what they had and the loop goes on.
        Err(_) => true,
    }
}

/// One frame in, falling back to a tick for anything unreadable.
///
/// A tick rather than a close: a frame this build cannot parse is a newer harness saying something
/// this one has no name for, and ending the surface over it would make every addition breaking.
fn read(line: &str) -> ToSurface {
    serde_json::from_str(line).unwrap_or(ToSurface::Tick)
}

/// One frame out.
fn say(frame: &FromSurface) {
    use std::io::Write;
    if let Ok(line) = serde_json::to_string(frame) {
        println!("{line}");
        // Flushed every frame. Buffered, a game's rows would arrive in batches and the surface
        // would look frozen and then jump.
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_frame_is_a_tick_rather_than_the_end() {
        // A newer harness saying something this build has no name for. Ending the surface over it
        // would make every addition to the protocol a breaking one.
        assert_eq!(read(r#"{"to":"nothing_yet"}"#), ToSurface::Tick);
        assert_eq!(read("not json at all"), ToSurface::Tick);
    }

    #[test]
    fn the_frames_this_build_knows_read_back_as_themselves() {
        assert_eq!(read(r#"{"to":"tick"}"#), ToSurface::Tick);
        assert_eq!(read(r#"{"to":"close"}"#), ToSurface::Close);
        // No `state` on the wire is a terminal that cannot tell a hold from a tap, which is most
        // of them and every one before the Kitty protocol.
        assert_eq!(
            read(r#"{"to":"key","key":"space"}"#),
            ToSurface::Key {
                key: "space".to_owned(),
                state: crate::tools::Held::Down,
            }
        );
        assert_eq!(
            read(r#"{"to":"key","key":"space","state":"up"}"#),
            ToSurface::Key {
                key: "space".to_owned(),
                state: crate::tools::Held::Up,
            }
        );
    }

    #[test]
    fn a_click_arrives_in_this_surface_own_rows() {
        // Already translated by the harness, which is the only thing that knows where the rows
        // landed. Row zero is this surface's first row, so a tenant needs no bounds check.
        assert_eq!(
            read(r#"{"to":"mouse","kind":"press","button":"left","row":2,"col":11}"#),
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
    ///
    /// The half of [`super::hold`] a frame test cannot reach: it is installed by that function
    /// rather than by the VM, so a refactor that dropped the one `lend` line would take the
    /// capability with it and every test here would still pass.
    #[test]
    fn a_surface_is_lent_the_question_a_run_is_not() {
        // Asserted in Lua, because that is the only place the `casper` table can be seen. A
        // failed `assert` raises, and a raise is what `run` reports.
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

        // The same call `hold` makes, so dropping it from there fails here.
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
        // The whole reason the tenant returns a closure: `n` lives in its upvalues, and nothing
        // out here has to know that a counter has an `n` or that a game has a dinosaur.
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
        // It asked for four rows and was told four. A tenant laid out for a size it guessed at
        // would draw once wrongly before anything corrected it.
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
        // What makes the rows a screen rather than a picture: a field somebody types into puts the
        // real cursor in itself, and an IME and a screen reader follow that rather than a block
        // the tenant painted.
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
        // Its rows can never be filled again, and holding them would leave a hole on the screen
        // that no key could close.
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
        // Every tool but a handful. Asked rather than required, so declaring one stays two keys.
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
