//! The one way a declaration reaches a process: `os.execute` and `io.popen` are gone — see
//! [`crate::lua::sandbox`] — and this replaces them. What starts the program, jailed, is
//! [`crate::running`].
//!
//! ```lua
//! local done = casper.exec("bat", { "--color=always", path })
//! casper.exec("sh", { "-c", 'cat > "$1"', "sh", path }, contents)  -- a third argument is stdin
//! ```

use luna::{Callback, CallbackReturn, Table, Value};

/// `casper.exec`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (program, args, input): (Value, Value, Value) = stack.consume(ctx)?;
        let input = match input {
            Value::String(fed) => Some(fed.as_bytes().to_vec()),
            _ => None,
        };
        let Value::String(program) = program else {
            return Err(raise(
                ctx,
                "casper.exec(program, args): a program name first",
            ));
        };
        let program = String::from_utf8_lossy(program.as_bytes()).into_owned();

        let mut argv: Vec<String> = Vec::new();
        if let Value::Table(args) = args {
            for nth in 1.. {
                match args.get_value(ctx, nth) {
                    Value::Nil => break,
                    Value::String(s) => {
                        argv.push(String::from_utf8_lossy(s.as_bytes()).into_owned())
                    }
                    other => argv.push(format!("{other:?}")),
                }
            }
        }

        let done = crate::running::fed(&program, &argv, input);
        let out = Table::new(&ctx);
        out.set(
            ctx,
            "out",
            luna::String::from_slice(&ctx, done.out.as_bytes()),
        )
        .ok();
        out.set(
            ctx,
            "err",
            luna::String::from_slice(&ctx, done.err.as_bytes()),
        )
        .ok();
        out.set(ctx, "code", done.code).ok();
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}
