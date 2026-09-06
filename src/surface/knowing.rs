//! `casper.knows(verb, args)` — what a surface may ask the harness about the session.
//!
//! ```lua
//! surface = function(args, size)
//!   return function(event)
//!     local who = casper.knows("session")                       -- { id = …, cwd = … }
//!     local found, why = casper.knows("memories", { query = "deploy", limit = 5 })
//!     if not found then return { lines = { line(why) } } end
//!     …
//!   end
//! end,
//! ```
//!
//! **This is the direction that did not exist.** Everything else a surface has, it was handed at
//! open: its rows, its width, the arguments of the call it belongs to. So a picker could not list
//! what this session remembers and a game could not name the model it was being played beside —
//! anything a tenant knew, somebody had to think of passing in.
//!
//! **The harness owns the list.** `session`, `model`, `memories` — read-only facts about the
//! session the person is already looking at. A verb it does not know comes back refused *by name*,
//! which is the difference between a tenant built against a newer harness saying so on screen and
//! one sitting there waiting.
//!
//! Two values back, in Lua's own idiom: the answer, or `nil` and why not. A tenant that ignores
//! the second gets `nil` and draws around it, which is the right failure for something that only
//! wanted to decorate a row with the model's name.

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
        let args = match args {
            Value::Nil => serde_json::Value::Null,
            other => {
                crate::lua::convert::json_from_lua(ctx, other, 0).unwrap_or(serde_json::Value::Null)
            }
        };

        match crate::surface::wonder(&verb, args) {
            Ok(said) => {
                stack.replace(ctx, crate::lua::convert::lua_from_json(ctx, &said));
            }
            // `nil, why` rather than a raise. A refusal is an ordinary answer — there is no
            // balthasar on this machine, no model is configured — and a tenant that ended over one
            // would take its rows down over something it could have drawn a sentence about.
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
