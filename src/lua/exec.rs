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

/// One read off a pipe.
const CHUNK: usize = 8 * 1024;

/// Run one program to completion.
#[must_use]
pub fn run(program: &str, args: &[String]) -> Done {
    // Wrapped in a kernel jail when a coordinator asked for one; unchanged otherwise. This is the
    // one place a declaration's command becomes a process, so it is the one place to contain it.
    let (program, args) = crate::jail::wrap(program, args);
    let (program, args) = (program.as_str(), args.as_slice());
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Without this the program is reparented to init the moment a magi is killed.
    crate::tied::running(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(why) => {
            crate::noted!("exec: {program} could not be run: {why}");
            return Done {
                out: String::new(),
                err: format!("{program} could not be run: {why}"),
                code: -1,
            };
        }
    };
    // Both pipes are read as they fill and only [`MOST`] bytes are held: collecting each stream
    // whole first would let the program pick how much memory casper takes, and reading neither
    // would block it on a full pipe.
    let piped = child.stdout.take();
    let reading = std::thread::spawn(move || kept(piped));
    let err = kept(child.stderr.take());
    let code = child.wait().ok().and_then(|it| it.code());
    let out = reading.join().unwrap_or_default();
    Done {
        out: said(&out.0, out.1),
        err: said(&err.0, err.1),
        code: code.map_or(-1, i64::from),
    }
}

/// Read `from` to its end, holding the first [`MOST`] bytes and counting the rest.
fn kept<R: std::io::Read>(from: Option<R>) -> (Vec<u8>, usize) {
    let (mut held, mut dropped) = (Vec::new(), 0);
    let Some(mut from) = from else {
        return (held, dropped);
    };
    let mut buffer = [0_u8; CHUNK];
    while let Ok(read) = from.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let room = MOST.saturating_sub(held.len()).min(read);
        held.extend_from_slice(&buffer[..room]);
        dropped += read - room;
    }
    (held, dropped)
}

/// What one stream came back as: what was held, and a line saying how much was not.
fn said(held: &[u8], dropped: usize) -> String {
    if dropped == 0 {
        return String::from_utf8_lossy(held).into_owned();
    }
    // Back to where a character last ended: the hold stops wherever the room ran out.
    let whole = match std::str::from_utf8(held) {
        Ok(_) => held.len(),
        Err(bad) => bad.valid_up_to(),
    };
    format!(
        "{}\n… {} more bytes, not shown",
        String::from_utf8_lossy(&held[..whole]),
        dropped + (held.len() - whole)
    )
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
        let done = run(
            "sh",
            &["-c".to_owned(), "head -c 600000 /dev/zero".to_owned()],
        );
        assert!(done.out.len() < 600_000, "{} bytes", done.out.len());
        assert!(
            done.out.ends_with("337856 more bytes, not shown"),
            "{}",
            &done.out[done.out.len() - 40..]
        );
    }

    #[test]
    fn what_fits_is_left_exactly_as_it_was() {
        assert_eq!(said(b"short", 0), "short");
    }

    #[test]
    fn cutting_lands_on_a_character_boundary() {
        // Three bytes to a character and `MOST` not a multiple of three, so the hold splits one.
        let huge = "€".repeat(MOST);
        let cut = said(&huge.as_bytes()[..MOST], 12);
        assert!(cut.starts_with('€'));
        assert!(!cut.contains('\u{fffd}'), "a character was split");
        assert!(
            cut.ends_with("13 more bytes, not shown"),
            "{}",
            &cut[cut.len() - 40..]
        );
    }

    /// This process's peak resident size, in kilobytes.
    fn peak_kb() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .and_then(|rest| rest.trim().trim_end_matches(" kB").trim().parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn a_program_that_writes_without_stopping_does_not_choose_caspers_memory() {
        let before = peak_kb();
        // Half a gigabyte: a fixture no buffer on this path could hold by accident.
        let done = run(
            "sh",
            &["-c".to_owned(), "head -c 536870912 /dev/zero".to_owned()],
        );
        let grew = peak_kb().saturating_sub(before);
        assert!(done.out.len() < MOST + 64, "{} bytes held", done.out.len());
        assert!(grew < 64 * 1024, "casper grew {grew} kB reading 512 MB");
    }
}
