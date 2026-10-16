//! The one way anything in casper reaches a process.
//!
//! Wrapped in the kernel jail a coordinator asked for, because this is where a command becomes a
//! process and so the only place to contain one. It sits under [`crate::lua::exec`] rather than
//! inside it so that a tool written in Rust — a listing, a search — reaches a program the same
//! way a declaration does, without the two modules depending on each other in a circle.

/// The most output one call will carry back.
///
/// Cut with a line saying so rather than silently: output that stops mid-sentence reads as a crash.
pub const MOST: usize = 256 * 1024;

/// The standard output of one program, at most `limit` bytes, read within [`READING`].
pub fn binary(program: &str, args: &[String], limit: usize) -> Result<Vec<u8>, String> {
    binary_by(program, args, limit, READING)
}

/// [`binary`], with the reader's process group killed and reaped once `limit` has passed.
fn binary_by(
    program: &str,
    args: &[String],
    most: usize,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let jail = crate::jail::Jail::from_env();
    let mut prepared = jail
        .prepare(program, args, None, &[], false)
        .map_err(|e| format!("jail refused: {e}"))?;
    let mut command = prepared.command();
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let ([(bytes, past), (err, _)], code, overran) = drained(&mut child, limit, [most, WHY]);
    let restored = prepared.restored();
    if !restored.is_empty() {
        return Err(restored.join("; "));
    }
    if overran {
        return Err(format!(
            "{program}: stopped after {}s, the most one read may take",
            limit.as_secs()
        ));
    }
    if past > 0 {
        return Err("image exceeds the source byte limit".into());
    }
    if code != Some(0) {
        let code = code.map_or_else(|| "killed".to_owned(), |c| c.to_string());
        let why = String::from_utf8_lossy(&err);
        return Err(match why.lines().next().filter(|line| !line.is_empty()) {
            Some(why) => format!("{why} (exit {code})"),
            None => format!("{program}: file read refused or failed (exit {code})"),
        });
    }
    Ok(bytes)
}

/// `program` fed `input`, answering its standard output: at most `most` bytes, within `limit`.
///
/// # Errors
/// The first line it wrote to standard error when it failed, or that it wrote too much or ran out
/// of time.
pub fn piped(
    program: &str,
    input: Vec<u8>,
    most: usize,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let jail = crate::jail::Jail::from_env();
    let mut prepared = jail
        .prepare(program, &[], None, &[], false)
        .map_err(|e| format!("jail refused: {e}"))?;
    let mut command = prepared.command();
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("{program} could not be run: {e}"))?;
    if let Some(mut pipe) = child.stdin.take() {
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = pipe.write_all(&input);
        });
    }
    let ([(out, past), (err, _)], code, overran) = drained(&mut child, limit, [most; 2]);
    let restored = prepared.restored();
    if !restored.is_empty() {
        return Err(restored.join("; "));
    }
    let why = String::from_utf8_lossy(&err)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    if overran {
        return Err(format!("stopped after {}s", limit.as_secs()));
    }
    if past > 0 {
        return Err(format!("wrote more than {most} bytes"));
    }
    match code {
        Some(0) => Ok(out),
        Some(code) if !why.is_empty() => Err(format!("{why} (exit {code})")),
        Some(code) => Err(format!("exited {code}")),
        None => Err("killed".to_owned()),
    }
}

/// One read off a pipe.
const CHUNK: usize = 8 * 1024;

/// How much of a reader's standard error is kept, for the line saying why it failed.
const WHY: usize = 4 * 1024;

/// How long one program may run before its process group is killed.
pub const DEADLINE: Duration = Duration::from_secs(60 * 60);

/// How long one read through [`binary`] may take before its process group is killed: a file that
/// never ends, such as a FIFO with no writer, is not waited on past this.
pub const READING: Duration = Duration::from_secs(30);

/// How long the pipes are still read once the program itself has exited: what a backgrounded
/// grandchild holds open is not waited for past this.
const GRACE: Duration = Duration::from_millis(250);

/// How often the loop looks at whether the program has exited.
const TICK: Duration = Duration::from_millis(50);

use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

/// What a program left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// Its standard output, bounded.
    pub out: String,
    /// Its standard error, bounded.
    pub err: String,
    /// Its exit status, or `-1` when it could not be started at all. A success whose change to a
    /// protected file the jail put back is `1`, and `err` says what was put back.
    pub code: i64,
}

/// Run one program to completion.
#[must_use]
pub fn run(program: &str, args: &[String]) -> Done {
    fed(program, args, None)
}

/// Run one program to completion with `input` on its standard input: how a declaration writes a
/// file, through a program inside the jail rather than by a hand that stands outside it.
#[must_use]
pub fn fed(program: &str, args: &[String], input: Option<Vec<u8>>) -> Done {
    fed_by(program, args, input, DEADLINE)
}

/// [`fed`], with the program's process group killed and reaped once `limit` has passed.
fn fed_by(program: &str, args: &[String], input: Option<Vec<u8>>, limit: Duration) -> Done {
    let began = Instant::now();
    let jail = crate::jail::Jail::from_env();
    let asked = crate::noted::command(program, args.len());
    let mut prepared = match jail.prepare(program, args, None, &[], false) {
        Ok(prepared) => prepared,
        Err(why) => {
            return Done {
                out: String::new(),
                err: format!("jail refused: {why}"),
                code: -1,
            };
        }
    };
    let mut command = prepared.command();
    let stdin = if input.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    };
    command
        .stdin(stdin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
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
    // Fed from its own thread, so a program writing while it reads cannot wedge on a full pipe.
    // Never joined: a grandchild holding stdin open must not hold this call.
    if let Some((bytes, mut pipe)) = input.zip(child.stdin.take()) {
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = pipe.write_all(&bytes);
        });
    }
    let (held, code, overran) = drained(&mut child, limit, [MOST; 2]);
    let restored = prepared.restored();
    let [out, err] = held;
    crate::noted!(
        "exec: {asked} jailed={} → exit {} in {}ms, {} bytes out, {} err",
        jail.on(),
        code.map_or_else(|| "killed".to_owned(), |c| c.to_string()),
        began.elapsed().as_millis(),
        out.0.len() + out.1,
        err.0.len() + err.1
    );
    let mut err = said(&err.0, err.1);
    if overran {
        err.push_str(&format!(
            "\n… stopped after {}s, the most one command may run",
            limit.as_secs()
        ));
    }
    let mut code = code.map_or(-1, i64::from);
    if !restored.is_empty() {
        if !err.is_empty() && !err.ends_with('\n') {
            err.push('\n');
        }
        err.push_str(&restored.join("\n"));
        if code == 0 {
            code = 1;
        }
    }
    Done {
        out: said(&out.0, out.1),
        err,
        code,
    }
}

/// What one pipe gave: the first bytes it had room for, and how many more there were.
type Kept = (Vec<u8>, usize);

/// Read both pipes as they fill, holding `most` bytes of standard output and of standard error,
/// until the program has exited and its pipes have closed, or [`GRACE`] after it exited, whichever
/// is first. Past `limit`, its whole process group is killed and the program reaped. Answers the
/// two holds, the exit code, and whether it overran.
fn drained(
    child: &mut std::process::Child,
    limit: Duration,
    most: [usize; 2],
) -> ([Kept; 2], Option<i32>, bool) {
    let mut pipes: [Option<OwnedFd>; 2] = [
        child.stdout.take().map(OwnedFd::from),
        child.stderr.take().map(OwnedFd::from),
    ];
    let mut held: [Kept; 2] = Default::default();
    let deadline = Instant::now() + limit;
    let mut ended: Option<(Option<i32>, Instant)> = None;
    loop {
        if ended.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            ended = Some((status.code(), Instant::now() + GRACE));
        }
        let until = ended.map_or(deadline, |(_, grace)| grace);
        let open = pipes.iter().any(Option::is_some);
        if (!open && ended.is_some()) || Instant::now() >= until {
            break;
        }
        let wait = until.saturating_duration_since(Instant::now()).min(TICK);
        if open {
            read_ready(&mut pipes, &mut held, wait, most);
        } else {
            std::thread::sleep(wait);
        }
    }
    if let Some((code, _)) = ended {
        return (held, code, false);
    }
    let group = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    let code = child.wait().ok().and_then(|status| status.code());
    (held, code, true)
}

/// Wait up to `wait` for either pipe to have something, and read what is there, holding as many
/// bytes of each as `most` gives it. A pipe at its end, or one that fails, is closed.
fn read_ready(
    pipes: &mut [Option<OwnedFd>; 2],
    held: &mut [Kept; 2],
    wait: Duration,
    most: [usize; 2],
) {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let readable = PollFlags::IN | PollFlags::HUP | PollFlags::ERR;
    let ready: Vec<bool> = {
        let mut fds: Vec<PollFd<'_>> = pipes
            .iter()
            .flatten()
            .map(|pipe| PollFd::new(pipe, readable))
            .collect();
        let timeout = Timespec::try_from(wait).unwrap_or_default();
        if rustix::event::poll(&mut fds, Some(&timeout)).is_err() {
            return;
        }
        fds.iter().map(|fd| !fd.revents().is_empty()).collect()
    };
    let mut ready = ready.into_iter();
    for ((pipe, kept), most) in pipes.iter_mut().zip(held.iter_mut()).zip(most) {
        let Some(fd) = pipe else { continue };
        if !ready.next().unwrap_or(false) {
            continue;
        }
        let mut buffer = [0_u8; CHUNK];
        match rustix::io::read(fd.as_fd(), &mut buffer) {
            Err(rustix::io::Errno::INTR) => {}
            Ok(0) | Err(_) => *pipe = None,
            Ok(read) => {
                let room = most.saturating_sub(kept.0.len()).min(read);
                kept.0.extend(buffer.iter().take(room));
                kept.1 += read - room;
            }
        }
    }
}

/// What one stream came back as: what was held, and a line saying how much was not.
fn said(held: &[u8], dropped: usize) -> String {
    if dropped == 0 {
        return String::from_utf8_lossy(held).into_owned();
    }
    // Back to where a character last ended: the hold stops wherever the room ran out.
    let valid = match std::str::from_utf8(held) {
        Ok(_) => held.len(),
        Err(bad) => bad.valid_up_to(),
    };
    let (whole, cut) = held.split_at_checked(valid).unwrap_or((held, &[]));
    format!(
        "{}\n… {} more bytes, not shown",
        String::from_utf8_lossy(whole),
        dropped + cut.len()
    )
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
    fn input_is_fed_to_the_program_and_closed() {
        // `cat` ends only when its input does, so this also proves the pipe is closed.
        let done = fed("cat", &[], Some(b"fed through\n".to_vec()));
        assert_eq!(done.out, "fed through\n");
        assert_eq!(done.code, 0);
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

    /// Whether `pid` is still a live process.
    fn alive(pid: &str) -> bool {
        std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z'))
        })
    }

    #[test]
    fn a_grandchild_holding_the_pipes_does_not_hold_the_call() {
        let began = Instant::now();
        let script = "sleep 20 & echo $!".to_owned();
        let done = run("sh", &["-c".to_owned(), script]);
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(done.code, 0, "{}", done.err);
        let pid = rustix::process::Pid::from_raw(done.out.trim().parse().expect("a pid"));
        if let Some(pid) = pid {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }

    #[test]
    fn past_its_deadline_a_program_and_its_group_are_killed_and_reaped() {
        let began = Instant::now();
        let script = "sleep 20 & echo $! > /dev/stderr; sleep 20".to_owned();
        let args = ["-c".to_owned(), script];
        let done = fed_by("sh", &args, None, Duration::from_millis(300));
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(done.code, -1);
        assert!(done.err.contains("stopped after"), "{}", done.err);
        let pid = done.err.lines().next().unwrap_or_default().to_owned();
        let gone = Instant::now() + Duration::from_secs(5);
        while alive(&pid) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(&pid), "the backgrounded {pid} outlived the deadline");
    }

    #[test]
    fn a_reader_that_never_finishes_is_killed_at_its_deadline() {
        let dir = crate::scratch::Scratch::new("running", "reader");
        let fifo = dir.join("never.png");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "{made}");
        let pidfile = dir.join("pid");
        let script = format!("sleep 20 & echo $! > '{}'; sleep 20", pidfile.display());
        let path = fifo.display().to_string();
        for (program, args) in [
            (
                "head",
                vec!["-c".to_owned(), "32".to_owned(), "--".to_owned(), path],
            ),
            ("sh", vec!["-c".to_owned(), script]),
        ] {
            let began = Instant::now();
            let read = binary_by(program, &args, 32, Duration::from_millis(300));
            assert!(
                began.elapsed() < Duration::from_secs(5),
                "{program}: {:?}",
                began.elapsed()
            );
            assert!(
                read.as_ref()
                    .is_err_and(|why| why.contains("stopped after")),
                "{program}: {read:?}"
            );
        }
        let pid = std::fs::read_to_string(&pidfile).expect("a pid");
        let gone = Instant::now() + Duration::from_secs(5);
        while alive(&pid) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(&pid), "the backgrounded {pid} outlived the deadline");
    }

    #[test]
    fn a_file_that_is_not_there_is_said_whole_however_little_was_asked() {
        let dir = crate::scratch::Scratch::new("running", "missing");
        let path = dir.join("nope.txt").display().to_string();
        let args = ["-c".to_owned(), "32".to_owned(), "--".to_owned(), path];
        let why = binary("head", &args, 32).expect_err("nothing to read");
        assert!(why.contains("No such file or directory"), "{why}");
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
        if crate::testing::isolated(
            concat!(
                module_path!(),
                "::a_program_that_writes_without_stopping_does_not_choose_caspers_memory"
            )
            .trim_start_matches("casper::"),
        ) {
            return;
        }
        let before = peak_kb();
        // Half a gigabyte: a fixture no buffer on this path could hold by accident.
        let done = run(
            "sh",
            &["-c".to_owned(), "head -c 536870912 /dev/zero".to_owned()],
        );
        let grew = peak_kb().saturating_sub(before);
        assert_eq!(done.code, 0, "the output fixture failed: {}", done.err);
        assert!(done.out.len() < MOST + 64, "{} bytes held", done.out.len());
        assert!(grew < 64 * 1024, "casper grew {grew} kB reading 512 MB");
    }
}
