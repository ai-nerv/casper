//! `casper.tapped(event)` — the key a person just pressed, or nothing.
//!
//! ```lua
//! return function(event)
//!   local key = casper.tapped(event)
//!   if key == "down" then at = at + 1
//!   elseif key == "enter" then return { answered = pick(at) } end
//!   return draw()
//! end
//! ```
//!
//! **What it is for.** Where the Kitty keyboard protocol is live a keystroke arrives *twice* —
//! once going down, once coming up — and nothing in the frame makes that obvious to somebody
//! writing their first surface. A list that acted on both moved two rows for one press of the
//! arrow, which is exactly the bug that shipped in the permission prompt: a person selecting the
//! wrong permission with no way to see why.
//!
//! Nothing in the protocol tells you which kind of tenant you are. A game *wants* the release,
//! because that is what ends a jump; a list has no use for it and cannot tell it apart from a
//! press. So the safe reading is the short one: ask for a tap and get a tap.
//!
//! **What it hides, and when not to use it.**
//!
//! - A release comes back as `nil`. A tenant that needs one — anything where holding a key means
//!   something — reads `event.state` itself. Both games do.
//! - A *repeat* comes back as the key. It says the key is still down, which for a list is another
//!   step: holding an arrow scrolls, rather than needing a tap a row.
//! - The name is folded to lower case, because the thing this is for is matching against `"j"`,
//!   `"enter"`, `"esc"`. A tenant that wants the character as typed — a field somebody is typing
//!   into — reads `event.key`, where a capital is still a capital.
//!
//! Anything that is not a key event at all — a tick, a resize, the pointer — is `nil`, so one
//! call answers "is this a keypress, and which" without a guard in front of it.

use luna::{Callback, CallbackReturn, Value};

/// `casper.tapped`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let event: Value = stack.consume(ctx)?;
        let answer = tapped(ctx, &event).map_or(Value::Nil, |name| {
            Value::String(luna::String::from_slice(&ctx, name.as_bytes()))
        });
        stack.replace(ctx, answer);
        Ok(CallbackReturn::Return)
    })
}

/// The key this event is a press of, lower-cased, or `None`.
fn tapped<'gc>(ctx: luna::Context<'gc>, event: &Value<'gc>) -> Option<String> {
    let Value::Table(event) = event else {
        // Not a table at all. `nil` rather than a raise: this sits at the top of a frame handler,
        // and a tenant handed something odd should draw its rows rather than end.
        return None;
    };
    let text = |field| match event.get_value(ctx, field) {
        Value::String(s) => Some(String::from_utf8_lossy(s.as_bytes()).into_owned()),
        _ => None,
    };
    if text("kind").as_deref() != Some("key") {
        return None;
    }
    // Absent is a press. That is every terminal without the protocol, where a key is one bare
    // event and there is no state on the frame at all — and a guard that read a missing field as
    // "not a press" would make those terminals answer nothing.
    match text("state").as_deref() {
        None | Some("down" | "repeat") => text("key").map(|key| key.to_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::lua::engine::Engine;

    /// What `casper.tapped` answers for one event, as a surface would see it.
    fn tapped(event: &serde_json::Value) -> serde_json::Value {
        let mut engine = Engine::new();
        engine
            .run(
                r#"casper.tool("t", { description = "d", parameters = {},
                     run = function() return casper.surface{ rows = 1, about = "x" } end,
                     surface = function() return function(event)
                       local key = casper.tapped(event)
                       return { lines = { { { role = "text", text = key or "<nothing>" } } } }
                     end end })"#,
                "tools.lua",
            )
            .expect("it loads");
        assert!(engine.open(
            "t",
            &serde_json::json!({}),
            &serde_json::json!({"rows": 1, "cols": 20})
        ));
        engine.frame(event).expect("it drew")["lines"][0][0]["text"].clone()
    }

    #[test]
    fn a_key_going_down_is_a_tap() {
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "down", "state": "down"})),
            "down"
        );
    }

    #[test]
    fn a_key_coming_up_is_not() {
        // The whole reason this exists. A list that read a release as a press moved two rows for
        // one press of the arrow, and nothing on screen said why.
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "down", "state": "up"})),
            "<nothing>"
        );
    }

    #[test]
    fn a_repeat_is_a_tap_because_the_key_is_still_down() {
        // What makes holding an arrow scroll a list rather than needing a tap a row.
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "j", "state": "repeat"})),
            "j"
        );
    }

    #[test]
    fn a_terminal_that_says_nothing_about_state_is_pressing_the_key() {
        // Every terminal without the Kitty protocol. Reading a missing field as "not a press"
        // would make those answer nothing at all.
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "enter"})),
            "enter"
        );
    }

    #[test]
    fn a_capital_matches_the_binding_it_was_typed_for() {
        // `q` quits whether or not shift was down. A tenant that wants the character as typed —
        // a field somebody is typing into — reads `event.key` instead.
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "Q", "state": "down"})),
            "q"
        );
    }

    #[test]
    fn nothing_that_is_not_a_keypress_is_one() {
        for other in [
            serde_json::json!({"kind": "tick"}),
            serde_json::json!({"kind": "resize", "rows": 4, "cols": 20}),
            serde_json::json!({"kind": "mouse", "what": "press", "row": 0, "col": 0}),
            serde_json::json!({"kind": "open"}),
        ] {
            assert_eq!(tapped(&other), "<nothing>", "{other}");
        }
    }
}
