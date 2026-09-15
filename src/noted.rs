//! What casper did, step by step, for whoever is watching. Off unless `$CASPER_DEBUG_LOG` or the
//! family's shared `$NERV_LOG` names a file. Every line is stamped with the time, the program and
//! its pid, so one file can hold the whole family's steps in the order they happened.

use std::time::{SystemTime, UNIX_EPOCH};

/// casper's own log.
pub const VARIABLE: &str = "CASPER_DEBUG_LOG";

/// The log the whole family shares: magi, balthasar, melchior and casper append to one file.
pub const FAMILY: &str = "NERV_LOG";

/// How much of an argument or a result goes into a line.
const LONGEST: usize = 120;

/// Whether anybody asked for a log.
#[must_use]
pub fn enabled() -> bool {
    std::env::var_os(VARIABLE).is_some() || std::env::var_os(FAMILY).is_some()
}

/// Append one stamped line to every log that is asked for, once when both name the same file.
/// Silent when none is, and silent when a file cannot be opened.
pub fn note(args: std::fmt::Arguments<'_>) {
    let mut targets: Vec<std::ffi::OsString> = [VARIABLE, FAMILY]
        .into_iter()
        .filter_map(std::env::var_os)
        .collect();
    targets.dedup();
    if targets.is_empty() {
        return;
    }
    let line = stamped(SystemTime::now(), std::process::id(), args);
    for path in targets {
        note_to(std::path::Path::new(&path), format_args!("{line}"));
    }
}

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

/// One line as the log holds it: `2026-09-15T14:03:07.123Z casper[42] area: message`.
#[must_use]
pub fn stamped(at: SystemTime, pid: u32, args: std::fmt::Arguments<'_>) -> String {
    let message = args.to_string().replace(['\n', '\r'], " ");
    format!("{} casper[{pid}] {message}", clock(at))
}

/// Text cut to one short line, for arguments and results that can run to pages.
#[must_use]
pub fn short(text: &str) -> String {
    let line = text.replace(['\n', '\r'], " ");
    if line.chars().count() <= LONGEST {
        return line;
    }
    let mut cut: String = line.chars().take(LONGEST).collect();
    cut.push('…');
    cut
}

/// A moment as UTC, to the millisecond, worked out without a calendar crate.
fn clock(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(since.as_secs()).unwrap_or(0);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days, after Howard Hinnant's algorithm.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// Write one line to casper's log and the family's, formatted like `println!`. A macro so the
/// arguments are not evaluated when nobody asked for a log.
#[macro_export]
macro_rules! noted {
    ($($arg:tt)*) => {
        if $crate::noted::enabled() {
            $crate::noted::note(format_args!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::{FAMILY, VARIABLE, note_to, short, stamped};
    use crate::scratch::{Scratch, ScratchFile};
    use std::time::{Duration, UNIX_EPOCH};

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
            std::env::var_os(VARIABLE).is_none() && std::env::var_os(FAMILY).is_none(),
            "the suite sets no log"
        );
        let at = log("quiet");
        noted!("nobody asked");
        assert!(!at.exists(), "{}", at.display());
    }

    #[test]
    fn a_line_is_stamped_with_the_time_the_program_and_its_pid_and_is_one_line() {
        let at = UNIX_EPOCH + Duration::from_millis(1_789_480_987_123);
        assert_eq!(
            stamped(at, 42, format_args!("run: read\nsrc/a.rs")),
            "2026-09-15T14:03:07.123Z casper[42] run: read src/a.rs"
        );
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert!(stamped(leap, 1, format_args!("x")).starts_with("2024-02-29T00:00:00.000Z"));
    }

    #[test]
    fn a_long_argument_is_cut_to_a_short_line() {
        let cut = short(&"a\n".repeat(200));
        assert!(!cut.contains('\n'));
        assert_eq!(cut.chars().count(), 121, "{cut}");
    }
}
