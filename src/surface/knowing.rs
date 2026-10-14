//! `casper.knows(verb, args)` — read-only facts about the session, from inside a surface. The
//! harness owns the verb list — `session`, `model`, `memories` — and refuses one it does not know
//! by name. Two values back, in Lua's own idiom: the answer, or `nil` and why not.

use luna::{Callback, CallbackReturn, Value};

/// `casper.knows`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (verb, args): (Value, Value) = stack.consume(ctx)?;
        let Value::String(verb) = verb else {
            return Err(raise(ctx, "casper.knows(verb, args): a verb first"));
        };
        let verb = String::from_utf8_lossy(verb.as_bytes()).into_owned();
        let asked = crate::lua::convert::json_from_lua(ctx, args)
            .and_then(|args| crate::surface::wonder(&verb, args));
        match asked {
            Ok(said) => {
                stack.replace(ctx, crate::lua::convert::lua_from_json(ctx, &said));
            }
            // `nil, why` rather than a raise: a refusal is an ordinary answer, not an end.
            Err(why) => {
                stack.replace(
                    ctx,
                    (
                        Value::Nil,
                        Value::String(luna::String::from_slice(&ctx, why.as_bytes())),
                    ),
                );
            }
        }
        Ok(CallbackReturn::Return)
    })
}

/// An error a declaration sees as its own.
fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}
