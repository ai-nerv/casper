//! `casper.dirs`, the listing pair a declaration reaches.
//!
//! ```lua
//! local out = casper.dirs.list(args.path or ".", { hidden = args.all })
//! ```
//!
//! The walk is jailed; what comes back is drawn in [`crate::listing`]. `tracked` asks for the
//! repository's own account of what is under there, where there is one.

use crate::listing::{self, Shown, Walked};
use luna::{Callback, CallbackReturn, Context, Table, Value};

/// Build the `dirs` table.
pub fn table<'gc>(ctx: Context<'gc>) -> Table<'gc> {
    let dirs = Table::new(&ctx);
    dirs.set(ctx, "list", walked(ctx, listing::list)).ok();
    dirs.set(ctx, "tree", walked(ctx, listing::tree)).ok();
    dirs
}

/// One of the two, as a callable: the same walk, drawn by whichever `draw` was handed in.
fn walked<'gc>(ctx: Context<'gc>, draw: fn(&str, &Walked) -> Shown) -> Callback<'gc> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (root, options): (Value, Value) = stack.consume(ctx)?;
        let root = match root {
            Value::String(root) => String::from_utf8_lossy(root.as_bytes()).into_owned(),
            _ => ".".to_owned(),
        };
        let asked = match options {
            Value::Table(asked) => Some(asked),
            _ => None,
        };
        let depth = asked
            .and_then(|asked| match asked.get_value(ctx, "depth") {
                Value::Integer(depth) => usize::try_from(depth).ok(),
                Value::Number(depth) => Some(depth as usize),
                _ => None,
            })
            .unwrap_or(1);
        let hidden = asked
            .is_some_and(|asked| matches!(asked.get_value(ctx, "hidden"), Value::Boolean(true)));
        let tracked = asked
            .is_some_and(|asked| matches!(asked.get_value(ctx, "tracked"), Value::Boolean(true)));

        let out = Table::new(&ctx);
        match listing::walk(&root, depth, hidden, tracked) {
            Ok(walked) => {
                let shown = draw(&root, &walked);
                text(ctx, &out, "said", &shown.said);
                text(ctx, &out, "brief", &shown.brief);
                out.set(ctx, "shown", crate::lua::paint::lines(ctx, &shown.lines))
                    .ok();
            }
            Err(why) => {
                text(ctx, &out, "said", &why);
                out.set(ctx, "failed", true).ok();
            }
        }
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

fn text<'gc>(ctx: Context<'gc>, out: &Table<'gc>, key: &'static str, value: &str) {
    out.set(ctx, key, luna::String::from_slice(&ctx, value.as_bytes()))
        .ok();
}
