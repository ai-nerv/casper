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
//! Where the Kitty keyboard protocol is live a keystroke arrives twice, going down and coming
//! up. A release answers `nil`, a repeat answers the key, the name is folded to lower case, and
//! anything that is not a key event answers `nil`. A tenant that needs the release, or the
//! character as typed, reads `event.state` and `event.key` itself.

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
        return None;
    };
    let text = |field| match event.get_value(ctx, field) {
        Value::String(s) => Some(String::from_utf8_lossy(s.as_bytes()).into_owned()),
        _ => None,
    };
    if text("kind").as_deref() != Some("key") {
        return None;
    }
    // Absent state is a press: that is every terminal without the Kitty protocol.
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
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "down", "state": "up"})),
            "<nothing>"
        );
    }

    #[test]
    fn a_repeat_is_a_tap_because_the_key_is_still_down() {
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "j", "state": "repeat"})),
            "j"
        );
    }

    #[test]
    fn a_terminal_that_says_nothing_about_state_is_pressing_the_key() {
        assert_eq!(
            tapped(&serde_json::json!({"kind": "key", "key": "enter"})),
            "enter"
        );
    }

    #[test]
    fn a_capital_matches_the_binding_it_was_typed_for() {
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
