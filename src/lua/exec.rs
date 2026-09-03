//! The one way a declaration reaches a process.
//!
//! `os.execute` and `io.popen` are gone — see [`crate::lua::sandbox`] — and this is what replaces
//! them. Not because running programs is dangerous here; running programs is casper's entire job.
//! Because a declaration that spawned directly would spawn *outside* everything casper is for:
//! no bound on the output, nothing to cancel, no verb attached, and no record of what ran.
//!
//! ```lua
//! local done = casper.exec("bat", { "--color=always", path })
//! if done.code ~= 0 then return { said = done.err, failed = true } end
//! return { said = done.out }
//! ```
//!
//! **Never on a socket.** This is reachable only from a declaration, and declarations run only on
//! the spawn link — argv and stdin, from a parent that could have run the command itself. A verb
//! that reached this over a socket would be a remote shell wearing a friendly name.

use luna::{Callback, CallbackReturn, Table, Value};

/// The most output one call will carry back.
///
/// A tool result is read by a model with a context window, so an unbounded one is a turn that
/// cannot be sent. Cut with a line saying so rather than silently: output that stops mid-sentence
/// reads as a program that crashed.
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
    ///
    /// A distinct number rather than an error, because "there is no `bat` on this machine" is
    /// something the *model* can act on — by asking for `cat` instead — and an error the caller
    /// had to translate would arrive as a broken tool.
    pub code: i64,
}

/// Run one program to completion.
#[must_use]
pub fn run(program: &str, args: &[String]) -> Done {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(done) => Done {
            out: bounded(&String::from_utf8_lossy(&done.stdout)),
            err: bounded(&String::from_utf8_lossy(&done.stderr)),
            code: done.status.code().map_or(-1, i64::from),
        },
        Err(why) => Done {
            out: String::new(),
            err: format!("{program} could not be run: {why}"),
            code: -1,
        },
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
        // A non-zero exit is a result the model reads, not an error the caller invents a message
        // for: what the program printed is usually what says how to fix it.
        let done = run("sh", &["-c".to_owned(), "echo oops >&2; exit 3".to_owned()]);
        assert_eq!(done.code, 3);
        assert_eq!(done.err.trim(), "oops");
    }

    #[test]
    fn a_program_that_is_not_installed_is_something_the_model_can_act_on() {
        // "there is no `bat` here" is answerable — ask for `cat` instead — and an error the
        // caller had to translate would reach the model as a broken tool.
        let done = run("casper-no-such-program-anywhere", &[]);
        assert_eq!(done.code, -1);
        assert!(done.err.contains("could not be run"), "{}", done.err);
    }

    #[test]
    fn output_is_cut_to_what_a_turn_can_carry_and_says_it_was() {
        // Silently stopping mid-sentence reads as a program that crashed.
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
        // A multi-byte character split down the middle is a string that will not build, which
        // would turn a long result into a panic.
        let huge = "é".repeat(MOST);
        let cut = bounded(&huge);
        assert!(cut.starts_with('é'));
    }
}
