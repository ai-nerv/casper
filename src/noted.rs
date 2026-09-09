//! What a spawned program said before it failed.
//!
//! A surface owns its pipes — stdout carries frames and stderr is thrown away by the harness —
//! so printing is not available for diagnosis. This is the only way anything in here can say
//! something to a person, and it is off unless the variable is set, because a tool that wrote a
//! file nobody asked for is a tool that fills a disk.
//!
//! It began as a private function in [`crate::pty`], which needed it first. It is here because
//! `lua/exec.rs` starts programs too, and a diagnostic that only covers one of the two places
//! that spawn is a diagnostic that is absent exactly when the other one is the problem.
//!
//! Deliberately not `tracing`. A handful of call sites do not justify a dependency, a
//! subscriber, an initialisation order and a second way to fail at start-up — and the siblings
//! may not depend on each other, so it would be four of them.

/// Append one line to `$CASPER_DEBUG_LOG`, if it is set.
///
/// Silent when it is not, and silent when the file cannot be opened: a diagnostic that could
/// fail the thing it is diagnosing is worse than no diagnostic.
pub fn note(args: std::fmt::Arguments<'_>) {
    let Some(path) = std::env::var_os(VARIABLE) else {
        return;
    };
    note_to(std::path::Path::new(&path), args);
}

/// Which variable turns this on.
pub const VARIABLE: &str = "CASPER_DEBUG_LOG";

/// The half that does not read the environment, so a test can exercise it.
///
/// Mutating `CASPER_DEBUG_LOG` from a test is not available: `set_var` is `unsafe` under this
/// edition and `unsafe` is denied across the workspace. Splitting the read from the write costs
/// one function and makes the writing testable, which is the half that can be wrong.
pub fn note_to(path: &std::path::Path, args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{args}");
    }
}

/// Write one line to `$CASPER_DEBUG_LOG`, formatted like `println!`.
///
/// A macro rather than a function so the arguments are not evaluated when the variable is unset,
/// which is every run but the one where somebody is looking.
#[macro_export]
macro_rules! noted {
    ($($arg:tt)*) => {
        if std::env::var_os($crate::noted::VARIABLE).is_some() {
            $crate::noted::note(format_args!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::{VARIABLE, note_to};
    use crate::scratch::{Scratch, ScratchFile};

    /// A log of this test's own, in a directory removed when the test returns *or* unwinds.
    ///
    /// This was a guard written out by hand here, on the grounds that casper had no scratch
    /// helper. It has one, and the hand-written copy was missing the counter that tells two
    /// fixtures of the same name apart and the delete-before-create that covers a pid coming
    /// round again.
    fn log(name: &str) -> ScratchFile {
        Scratch::file("casper-noted", name, "log.txt")
    }

    #[test]
    fn lines_are_appended_rather_than_replacing_each_other() {
        // One run of a session writes several; a log that kept only the last would answer
        // "what happened" with "the last thing".
        let at = log("append");
        note_to(&at, format_args!("{} exited {}", "models", 1));
        note_to(&at, format_args!("and again"));
        let held = std::fs::read_to_string(&at).expect("the log");
        assert_eq!(held, "models exited 1\nand again\n");
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_not_an_error() {
        // A diagnostic that fails the thing it is diagnosing is worse than no diagnostic.
        note_to(
            std::path::Path::new("/proc/nonexistent/nope"),
            format_args!("into the void"),
        );
    }

    #[test]
    fn nothing_is_written_when_nobody_asked() {
        // The macro is the guard: with the variable unset it does not reach `note` at all, and
        // no file appears anywhere. This is every run but the one where somebody is looking.
        assert!(
            std::env::var_os(VARIABLE).is_none(),
            "the suite sets no log"
        );
        let at = log("quiet");
        noted!("nobody asked");
        assert!(!at.exists(), "{}", at.display());
    }
}
