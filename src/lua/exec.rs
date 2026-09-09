//! The one way a declaration reaches a process.
//!
//! `os.execute` and `io.popen` are gone — see [`crate::lua::sandbox`] — and this replaces them.
//!
//! ```lua
//! local done = casper.exec("bat", { "--color=always", path })
//! if done.code ~= 0 then return { said = done.err, failed = true } end
//! return { said = done.out }
//! ```
//!
//! Reachable only from a declaration, which runs only on the spawn link and never on a socket.

use luna::{Callback, CallbackReturn, Table, Value};

/// The most output one call will carry back.
///
/// Cut with a line saying so rather than silently: output that stops mid-sentence reads as a crash.
pub const MOST: usize = 256 * 1024;

/// `casper.exec`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (program, args): (Value, Value) = stack.consume(ctx)?;
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

        let done = run(&program, &argv);
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

/// What a program left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// Its standard output, bounded.
    pub out: String,
    /// Its standard error, bounded.
    pub err: String,
    /// Its exit status, or `-1` when it could not be started at all.
    pub code: i64,
}

/// Run one program to completion.
#[must_use]
pub fn run(program: &str, args: &[String]) -> Done {
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null());
    // Without this the program is reparented to init the moment a magi is killed.
    crate::tied::running(&mut command);
    let out = command.output();
    match out {
        Ok(done) => Done {
            out: bounded(&String::from_utf8_lossy(&done.stdout)),
            err: bounded(&String::from_utf8_lossy(&done.stderr)),
            code: done.status.code().map_or(-1, i64::from),
        },
        Err(why) => {
            crate::noted!("exec: {program} could not be run: {why}");
            Done {
                out: String::new(),
                err: format!("{program} could not be run: {why}"),
                code: -1,
            }
        }
    }
}

/// Cut `text` to what a turn can carry, saying so if anything went.
fn bounded(text: &str) -> String {
    if text.len() <= MOST {
        return text.to_owned();
    }
    // On a character boundary, or the string will not build.
    let mut at = MOST;
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    let dropped = text.len() - at;
    format!("{}\n… {dropped} more bytes, not shown", &text[..at])
}

/// Raise a message into Lua.
fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_works_answers_with_what_it_wrote() {
        let done = run("echo", &["hello".to_owned()]);
        assert_eq!(done.out.trim(), "hello");
        assert_eq!(done.code, 0);
    }

    #[test]
    fn a_program_that_failed_still_answers() {
        let done = run("sh", &["-c".to_owned(), "echo oops >&2; exit 3".to_owned()]);
        assert_eq!(done.code, 3);
        assert_eq!(done.err.trim(), "oops");
    }

    #[test]
    fn a_program_that_is_not_installed_is_something_the_model_can_act_on() {
        let done = run("casper-no-such-program-anywhere", &[]);
        assert_eq!(done.code, -1);
        assert!(done.err.contains("could not be run"), "{}", done.err);
    }

    #[test]
    fn output_is_cut_to_what_a_turn_can_carry_and_says_it_was() {
        let huge = "x".repeat(MOST + 500);
        let cut = bounded(&huge);
        assert!(cut.len() < huge.len());
        assert!(
            cut.ends_with("more bytes, not shown"),
            "{}",
            &cut[cut.len() - 40..]
        );
    }

    #[test]
    fn what_fits_is_left_exactly_as_it_was() {
        assert_eq!(bounded("short"), "short");
    }

    #[test]
    fn cutting_lands_on_a_character_boundary() {
        let huge = "é".repeat(MOST);
        let cut = bounded(&huge);
        assert!(cut.starts_with('é'));
    }
}
