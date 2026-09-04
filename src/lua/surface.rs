//! Asking the harness for rows to fill.
//!
//! ```lua
//! run = function(args)
//!   return casper.surface({ rows = 8, about = "the dinosaur game", tick = 60 })
//! end,
//!
//! surface = function(args, size)
//!   local state = { … }
//!   return function(event)          -- { kind = "key" | "tick" | "resize" | "open", … }
//!     …
//!     return { lines = { { { role = "text", text = "…" } } } }
//!     -- or:  return { answered = "quit" }
//!   end
//! end,
//! ```
//!
//! **A surface is space, not a question.** [`crate::lua::ask`] asks in a shape the harness chose —
//! a line of text and a list of options — and a tool that wanted to ask differently could not. A
//! surface asks only for *rows*: what goes in them is the tenant's, and the harness cannot tell a
//! permission prompt from a file picker from a game.
//!
//! **It is a renderer, never an authority.** What comes back at the end is the id the tenant drew,
//! not a decision — see DESIGN.md. A surface that could return "allowed" would be a sibling
//! granting itself a permission, and the ledger would be a suggestion.

use luna::{Callback, CallbackReturn, Table, Value};

/// `casper.surface`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let asked: Value = stack.consume(ctx)?;
        let Value::Table(asked) = asked else {
            return Err(raise(ctx, "casper.surface{ rows = …, about = … }: a table"));
        };

        let rows = match asked.get_value(ctx, "rows") {
            Value::Integer(rows) if rows > 0 => rows,
            // A surface with no height is a tool that asked for nothing and would then be handed
            // nothing to draw in, which reads to a person as a call that hung.
            _ => {
                return Err(raise(
                    ctx,
                    "casper.surface: `rows` must be a positive number of rows",
                ));
            }
        };

        let surface = Table::new(&ctx);
        surface
            .set(ctx, "shown", luna::String::from_slice(&ctx, b"surface"))
            .ok();
        surface.set(ctx, "rows", rows).ok();
        // What it is for, for a harness with no screen. `magi -p` cannot draw rows and cannot ask
        // anybody; it says this and declines, rather than waiting on a surface nobody will fill.
        let about = match asked.get_value(ctx, "about") {
            Value::String(about) => about,
            _ => luna::String::from_slice(&ctx, b"a tool wants the screen"),
        };
        surface.set(ctx, "about", about).ok();
        // Only for one that moves on its own. A picker redraws when a key arrives and at no other
        // time, and ticking it would be a wakeup many times a second to draw the same rows.
        if let Value::Integer(tick) = asked.get_value(ctx, "tick")
            && tick > 0
        {
            surface.set(ctx, "tick", tick).ok();
        }

        // The whole result, like `casper.ask`: a declaration writing `return casper.surface(…)`
        // is saying "this call is not finished", and wrapping it here is what makes that one line.
        let out = Table::new(&ctx);
        out.set(ctx, "shown", surface).ok();
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

/// A raise a declaration can read.
fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use crate::lua::engine::Engine;

    /// What a declaration returning a surface produces.
    fn asked(source: &str) -> serde_json::Value {
        let mut engine = Engine::new();
        engine.run(source, "tools.lua").expect("it loads");
        let ran = engine.call("t", &serde_json::json!({})).expect("it runs");
        serde_json::to_value(ran).expect("encodes")
    }

    #[test]
    fn a_surface_asks_for_rows_and_says_what_it_is_for() {
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ rows = 8, about = "a game", tick = 60 } end })"#,
        );
        assert_eq!(out["shown"]["shown"], "surface");
        assert_eq!(out["shown"]["rows"], 8);
        assert_eq!(out["shown"]["about"], "a game");
        assert_eq!(out["shown"]["tick"], 60);
    }

    #[test]
    fn a_surface_that_does_not_move_asks_for_no_tick() {
        // A picker redraws on a keypress and at no other time. Ticking it would wake the whole
        // session many times a second to draw exactly the same rows.
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ rows = 3, about = "pick one" } end })"#,
        );
        assert!(out["shown"].get("tick").is_none(), "{out}");
    }

    #[test]
    fn asking_for_no_rows_is_refused_rather_than_drawn_empty() {
        // It would be handed nothing to draw in, and a person would read that as a hang.
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ about = "nothing" } end })"#,
        );
        assert_eq!(out["failed"], true, "{out}");
        assert!(out["said"].as_str().unwrap_or_default().contains("rows"));
    }
}
