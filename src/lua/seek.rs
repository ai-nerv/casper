//! `casper.seek`, the semantic search a declaration reaches.
//!
//! ```lua
//! local out = casper.seek(args.query, args.path, { limit = args.limit }, args.answered)
//! if out.wonder then return casper.wonder("helper", out.wonder, out.about) end
//! ```
//!
//! One call for both halves of a search: without `answered` it comes back with the question to
//! put to the harness, with it it comes back with the result. See [`crate::seeking`].

use crate::seeking::{Asked, Sought};
use luna::{Callback, CallbackReturn, Table, Value};

/// How many passages a search shows when the call did not say.
const SHOWN: usize = 8;

/// `casper.seek`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (query, root, options, answered): (Value, Value, Value, Value) = stack.consume(ctx)?;
        let Value::String(query) = query else {
            return Err(raise(ctx, "casper.seek(query, path): a question first"));
        };
        let query = String::from_utf8_lossy(query.as_bytes()).into_owned();
        let root = text_of(root).unwrap_or_else(|| ".".to_owned());
        let answered = text_of(answered);
        let limit = match options {
            Value::Table(asked) => match asked.get_value(ctx, "limit") {
                Value::Integer(limit) => usize::try_from(limit).unwrap_or(SHOWN),
                Value::Number(limit) => limit as usize,
                _ => SHOWN,
            },
            _ => SHOWN,
        };

        let out = Table::new(&ctx);
        let sought = crate::seeking::search(&Asked {
            query: &query,
            root: &root,
            limit,
            answered: answered.as_deref(),
        });
        match sought {
            Sought::Found(found) => {
                text(ctx, &out, "said", &found.said);
                text(ctx, &out, "brief", &found.brief);
                out.set(ctx, "shown", crate::lua::paint::lines(ctx, &found.lines))
                    .ok();
            }
            Sought::Ask(wonder, about) => {
                out.set(
                    ctx,
                    "wonder",
                    crate::lua::convert::lua_from_json(ctx, &wonder),
                )
                .ok();
                text(ctx, &out, "about", &about);
            }
            Sought::Failed(why) => {
                text(ctx, &out, "said", &why);
                out.set(ctx, "failed", true).ok();
            }
        }
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

fn text_of(value: Value<'_>) -> Option<String> {
    match value {
        Value::String(held) if !held.as_bytes().is_empty() => {
            Some(String::from_utf8_lossy(held.as_bytes()).into_owned())
        }
        _ => None,
    }
}

fn text<'gc>(ctx: luna::Context<'gc>, out: &Table<'gc>, key: &'static str, value: &str) {
    out.set(ctx, key, luna::String::from_slice(&ctx, value.as_bytes()))
        .ok();
}

fn raise<'gc>(ctx: luna::Context<'gc>, why: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        why.as_bytes(),
    )))
}
