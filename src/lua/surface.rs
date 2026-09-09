//! Asking the harness for rows to fill.
//!
//! ```lua
//! run = function(args)
//!   return casper.surface{ rows = 8, about = "the dinosaur game", tick = 60 }
//! end,
//! surface = function(args, size)
//!   return function(event)  -- kind = "key" | "tick" | "resize" | "open"
//!     return { lines = { { { role = "text", text = "…" } } } }  -- or { answered = "quit" }
//!   end
//! end,
//! ```
//!
//! A surface asks for rows; what goes in them is the tenant's. What comes back at the end is the
//! id the tenant drew, never a decision: a surface returning "allowed" would be a sibling
//! granting itself a permission.

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
        // Read by a harness with no screen, which declines with it rather than waiting on rows.
        let about = match asked.get_value(ctx, "about") {
            Value::String(about) => about,
            _ => luna::String::from_slice(&ctx, b"a tool wants the screen"),
        };
        surface.set(ctx, "about", about).ok();
        if let Value::Integer(tick) = asked.get_value(ctx, "tick")
            && tick > 0
        {
            surface.set(ctx, "tick", tick).ok();
        }

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
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ rows = 3, about = "pick one" } end })"#,
        );
        assert!(out["shown"].get("tick").is_none(), "{out}");
    }

    #[test]
    fn a_tool_that_runs_a_program_in_its_rows_is_given_a_tick() {
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ rows = 8, about = "htop" } end,
                 screen = function() return { command = "htop" } end })"#,
        );
        assert_eq!(out["shown"]["tick"], 33, "{out}");
    }

    #[test]
    fn a_screen_that_named_its_own_rate_keeps_it() {
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ rows = 8, about = "a clock", tick = 500 } end,
                 screen = function() return { command = "date" } end })"#,
        );
        assert_eq!(out["shown"]["tick"], 500, "{out}");
    }

    #[test]
    fn a_drawing_is_still_not_ticked_unless_it_asked() {
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ rows = 3, about = "pick one" } end,
                 surface = function() return function() return {} end end })"#,
        );
        assert!(out["shown"].get("tick").is_none(), "{out}");
    }

    #[test]
    fn asking_for_no_rows_is_refused_rather_than_drawn_empty() {
        let out = asked(
            r#"casper.tool("t", { description = "d", parameters = {},
                 run = function() return casper.surface{ about = "nothing" } end })"#,
        );
        assert_eq!(out["failed"], true, "{out}");
        assert!(out["said"].as_str().unwrap_or_default().contains("rows"));
    }
}
