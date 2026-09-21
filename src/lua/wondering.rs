//! Putting a question to the harness, from a declaration that has not finished.
//!
//! ```lua
//! if not args.answered then
//!   return casper.wonder("helper", { role = "summary", instruction = "…" }, "scoring")
//! end
//! ```
//!
//! [`crate::lua::ask`] is the same shape with a person at the other end. This one has the harness:
//! it answers out of the session and runs the call again with what it said. A harness with nothing
//! to answer with refuses, and the answer arrives as `{"refused": "…"}` rather than as silence.

use luna::{Callback, CallbackReturn, Table, Value};

/// `casper.wonder`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (wonder, args, about): (Value, Value, Value) = stack.consume(ctx)?;
        let Value::String(wonder) = wonder else {
            return Err(raise(
                ctx,
                "casper.wonder(verb, args): one of the harness's verbs first",
            ));
        };
        let wonder = String::from_utf8_lossy(wonder.as_bytes()).into_owned();

        let held = Table::new(&ctx);
        text(ctx, &held, "shown", "wonder");
        text(ctx, &held, "wonder", &wonder);
        held.set(ctx, "args", args).ok();
        if let Value::String(about) = about {
            held.set(ctx, "about", about).ok();
        }
        let out = Table::new(&ctx);
        out.set(ctx, "shown", held).ok();
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

fn text<'gc>(ctx: luna::Context<'gc>, held: &Table<'gc>, key: &'static str, value: &str) {
    held.set(ctx, key, luna::String::from_slice(&ctx, value.as_bytes()))
        .ok();
}

fn raise<'gc>(ctx: luna::Context<'gc>, why: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        why.as_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use crate::lua::engine::Engine;
    use crate::tools::Shown;

    fn ran(expression: &str) -> crate::tools::Ran {
        let mut engine = Engine::new();
        engine
            .run(
                &format!("casper.tool(\"probe\", {{ run = function() return {expression} end }})"),
                "test.lua",
            )
            .expect("the chunk runs");
        engine
            .call("probe", &serde_json::Value::Null)
            .expect("the tool ran")
    }

    #[test]
    fn a_question_for_the_harness_is_not_a_result() {
        let ran = ran("casper.wonder('helper', { role = 'summary' }, 'scoring')");
        assert!(ran.waiting(), "{ran:?}");
        assert!(ran.said.is_empty());
        let Some(Shown::Wonder(wondering)) = ran.shown else {
            panic!("a wonder travels as a wonder: {:?}", ran.shown);
        };
        assert_eq!(wondering.wonder, "helper");
        assert_eq!(wondering.args["role"], "summary");
        assert_eq!(wondering.about, "scoring");
    }

    #[test]
    fn a_wonder_with_nothing_to_say_still_travels() {
        let ran = ran("casper.wonder('session')");
        let Some(Shown::Wonder(wondering)) = ran.shown else {
            panic!("{:?}", ran.shown);
        };
        assert_eq!(wondering.wonder, "session");
        assert!(wondering.about.is_empty());
        let wire = serde_json::to_string(&wondering).expect("encodes");
        assert!(!wire.contains("about"), "{wire}");
        assert!(!wire.contains("args"), "{wire}");
    }
}
