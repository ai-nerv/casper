//! What a spawned program said before it failed.
//!
//! A surface owns its pipes — stdout carries frames, stderr is discarded — so nothing in here can
//! print for diagnosis. Off unless `$CASPER_DEBUG_LOG` is set.

/// Append one line to `$CASPER_DEBUG_LOG`, if it is set. Silent when it is not, and silent when
/// the file cannot be opened.
pub fn note(args: std::fmt::Arguments<'_>) {
    let Some(path) = std::env::var_os(VARIABLE) else {
        return;
    };
    note_to(std::path::Path::new(&path), args);
}

pub const VARIABLE: &str = "CASPER_DEBUG_LOG";

/// The half that does not read the environment, so a test can exercise it: `set_var` is `unsafe`
/// under this edition and `unsafe` is denied here, so no test can set the variable.
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

/// Write one line to `$CASPER_DEBUG_LOG`, formatted like `println!`. A macro so the arguments are
/// not evaluated when the variable is unset.
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
    fn log(name: &str) -> ScratchFile {
        Scratch::file("casper-noted", name, "log.txt")
    }

    #[test]
    fn lines_are_appended_rather_than_replacing_each_other() {
        let at = log("append");
        note_to(&at, format_args!("{} exited {}", "models", 1));
        note_to(&at, format_args!("and again"));
        let held = std::fs::read_to_string(&at).expect("the log");
        assert_eq!(held, "models exited 1\nand again\n");
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_not_an_error() {
        note_to(
            std::path::Path::new("/proc/nonexistent/nope"),
            format_args!("into the void"),
        );
    }

    #[test]
    fn nothing_is_written_when_nobody_asked() {
        assert!(
            std::env::var_os(VARIABLE).is_none(),
            "the suite sets no log"
        );
        let at = log("quiet");
        noted!("nobody asked");
        assert!(!at.exists(), "{}", at.display());
    }
}
