//! The one way a declaration reaches a process: `os.execute` and `io.popen` are gone — see
//! [`crate::lua::sandbox`] — and this replaces them. What starts the program, jailed, is
//! [`crate::running`].
//!
//! ```lua
//! local done = casper.exec("bat", { "--color=always", path })
//! casper.exec("sh", { "-c", 'cat > "$1"', "sh", path }, contents)  -- a third argument is stdin
//! casper.exec("casper", { "code", path })  -- this casper, run inside the same jail
//! ```

use luna::{Callback, CallbackReturn, Table, Value};

pub fn replace_source(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (path, text): (Value, Value) = stack.consume(ctx)?;
        let result = match (path, text) {
            (Value::String(path), Value::String(text)) => {
                let values = std::str::from_utf8(path.as_bytes())
                    .and_then(|path| std::str::from_utf8(text.as_bytes()).map(|text| (path, text)));
                values
                    .map_err(|_| "source path and text must be UTF-8".to_owned())
                    .and_then(|(path, text)| {
                        crate::source::replace(path, text).map_err(|why| why.to_string())
                    })
            }
            _ => Err("casper.replace_source(path, text): a path and text first".to_owned()),
        };
        match result {
            Ok(()) => stack.replace(ctx, (Value::Boolean(true), Value::Nil)),
            Err(why) => stack.replace(
                ctx,
                (
                    Value::Nil,
                    Value::String(luna::String::from_slice(&ctx, why.as_bytes())),
                ),
            ),
        }
        Ok(CallbackReturn::Return)
    })
}

pub fn source(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let path: Value = stack.consume(ctx)?;
        let result = match path {
            Value::String(path) => std::str::from_utf8(path.as_bytes())
                .map_err(|_| "source path must be UTF-8".to_owned())
                .and_then(|path| crate::source::jailed(path).map_err(|why| why.to_string())),
            _ => Err("casper.source(path): a file path first".to_owned()),
        };
        match result {
            Ok(text) => stack.replace(
                ctx,
                (
                    Value::String(luna::String::from_slice(&ctx, text.as_bytes())),
                    Value::Nil,
                ),
            ),
            Err(why) => stack.replace(
                ctx,
                (
                    Value::Nil,
                    Value::String(luna::String::from_slice(&ctx, why.as_bytes())),
                ),
            ),
        }
        Ok(CallbackReturn::Return)
    })
}

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
        let program = itself(String::from_utf8_lossy(program.as_bytes()).into_owned());

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

/// `casper` is this executable, whichever one `PATH` would have found first.
fn itself(program: String) -> String {
    if program != "casper" {
        return program;
    }
    std::env::current_exe().map_or(program, |own| own.display().to_string())
}

fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}
