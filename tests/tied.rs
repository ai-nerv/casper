//! Whether the kernel takes a casper with the process that started it.
//!
//! Against the real binary, because there is nothing to test below it: `PR_SET_PDEATHSIG` is a
//! property of a live process and its parent, and a unit test could only assert that a function
//! calls a function.
//!
//! The case that matters is the caller that is *killed*, not the one that exits. A caller with a
//! way out can end its children on the way; the ones that leave a casper running are the panic,
//! the `kill -9` and the OOM, where nothing in the caller runs at all. And casper is at its most
//! exposed in the middle of a call: the call arrives on stdin and stdin is read to end of file
//! before the tool starts, so from then on there is no pipe left for anybody to close.
//!
//! **And whether it takes the program it was running with it.** `PR_SET_PDEATHSIG` is cleared
//! across `fork`, so the tie casper sets for itself stops at casper: for a while the caller's
//! death killed casper and left its command reparented to init. Both shapes are checked, because
//! they are covered by different things — an ordinary program on a pty goes when the master
//! closes and the kernel hangs the session up, so the one asked for here is a program that
//! *ignores* the hangup, which nothing but the death signal will end.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for a process to notice its parent is gone.
///
/// The signal is immediate; this covers the scheduler getting round to the process and the test
/// getting round to looking. Generous, because a slow machine failing this would report the
/// guarantee as broken when it is only late.
const NOTICES_WITHIN: Duration = Duration::from_secs(10);

/// The binary under test, named once because every caller here is a shell script.
const CASPER: &str = env!("CARGO_BIN_EXE_casper");

/// A parent that starts one `casper run` and then does nothing at all.
///
/// A shell rather than this process: the parent has to be something the test can kill outright,
/// and killing the test runner is not available. `$!` is the pid of the casper rather than of
/// the shell, which is what has to be watched — a shell that dies takes nothing with it by
/// default, and that is the whole exercise.
struct Caller {
    shell: Child,
    served: u32,
    /// The command casper is running, so the test leaves nothing behind either.
    ran: Option<u32>,
    dir: std::path::PathBuf,
}

impl Caller {
    /// Start a shell that starts a casper on a call that will still be running.
    fn starting(name: &str) -> Self {
        // A regular file for stdin, so it is at end of file before the command even starts.
        // Nothing is left on the pipe for a dying caller to close, which is the gap this covers.
        Self::spawning(
            name,
            r#"{"tool":"shell","args":{"command":"sleep 30"}}"#,
            |frame| format!("{CASPER} run <{}", frame.display()),
        )
    }

    /// The same, for a program on a pty rather than a command run to completion.
    ///
    /// The program ignores `SIGHUP` on purpose. Every other one goes when the master closes and
    /// the session is hung up, which would make this pass against a casper that had never armed
    /// anything — the hangup and the death signal cover different programs and the test has to
    /// be about the second.
    ///
    /// A pipe for stdin rather than a file, because a surface is frames until the harness stops
    /// sending them: on end of file the loop is simply over, and the casper would exit on its
    /// own rather than being taken.
    fn on_a_screen(name: &str) -> Self {
        Self::spawning(
            name,
            r#"{"event":"open","rows":10,"cols":40,"args":{"command":"trap \"\" HUP; sleep 30"}}"#,
            |frame| {
                format!(
                    "{{ cat {}; sleep 30; }} | {CASPER} surface screen",
                    frame.display()
                )
            },
        )
    }

    fn spawning(name: &str, frame: &str, running: impl Fn(&Path) -> String) -> Self {
        let dir = std::env::temp_dir().join(format!("casper-tied-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let call = dir.join("call.json");
        // Newline-terminated: `run` reads to end of file and would not care, but a surface reads
        // lines and an unterminated one leaves it waiting for the rest of a frame that is all
        // there is.
        std::fs::write(&call, format!("{frame}\n")).expect("wrote");

        let pids = dir.join("pid");
        let script = format!(
            "{casper} >/dev/null 2>&1 & echo $! > {pids}; wait",
            casper = running(&call),
            pids = pids.display(),
        );
        let shell = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the caller");
        let served = read_pid(&pids).expect("the caller said which casper it started");
        let ran = running_under(served);
        Self {
            shell,
            served,
            ran,
            dir,
        }
    }

    /// End the caller the way a crash would: with nothing running inside it.
    fn killed(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }

    /// Leave nothing running and nothing on disk, whatever the assertions are about to do.
    fn cleared(mut self) {
        self.killed();
        for pid in [Some(self.served), self.ran].into_iter().flatten() {
            end(pid);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The pid the shell wrote down, once it has written it.
fn read_pid(at: &Path) -> Option<u32> {
    for _ in 0..250 {
        if let Ok(text) = std::fs::read_to_string(at)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// The first child of `pid`, once it has one.
///
/// Read out of `/proc` rather than tracked, because the process that spawned it is not this one.
fn running_under(pid: u32) -> Option<u32> {
    for _ in 0..250 {
        if let Ok(threads) = std::fs::read_dir(format!("/proc/{pid}/task")) {
            for thread in threads.flatten() {
                if let Ok(listed) = std::fs::read_to_string(thread.path().join("children"))
                    && let Some(first) = listed.split_whitespace().next()
                    && let Ok(child) = first.parse::<u32>()
                {
                    return Some(child);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Whether a process exists and is not merely a corpse waiting to be reaped.
///
/// The state field rather than the directory's existence: every process here is started by a
/// shell that is about to be killed, so a zombie is the expected shape of "gone".
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|stat| stat.split_whitespace().nth(2) != Some("Z"))
}

/// Wait for `pid` to go away, and say whether it did.
fn gone_within(pid: u32, patience: Duration) -> bool {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !alive(pid)
}

/// Leave nothing running, whatever the assertions did.
fn end(pid: u32) {
    let _ = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        // Already gone is the passing case, and its complaint reads like a failure.
        .stderr(Stdio::null())
        .status();
}

#[test]
fn a_call_does_not_outlive_the_process_that_asked_for_it() {
    // The guarantee. Nothing runs in a process that is killed outright, so the caller cannot be
    // what enforces this — and casper's own way of noticing, the call on stdin, ran out the
    // moment the call finished arriving.
    let mut caller = Caller::starting("killed");
    let served = caller.served;
    // The program the call is running. One level further down than the signal reaches on its
    // own, and the level a tool call actually costs something at: this is the build, the fetch,
    // the thing that was still going when the magi was killed.
    let ran = caller.ran.expect("the casper started what it was given");
    assert!(
        alive(served),
        "it is up while the caller that started it is"
    );

    caller.killed();
    let went = gone_within(served, NOTICES_WITHIN);
    let ran_went = gone_within(ran, NOTICES_WITHIN);

    caller.cleared();
    assert!(
        went,
        "a casper must not outlive the process that started it"
    );
    assert!(
        ran_went,
        "a command must not outlive the casper that started it"
    );
}

#[test]
fn a_program_on_a_screen_does_not_outlive_it_either() {
    // A pty covers the ordinary case by itself — the master closes, the session is hung up, the
    // program ends. This one has said it will not take a hangup, so what is left is the death
    // signal or nothing.
    let mut caller = Caller::on_a_screen("screened");
    let ran = caller.ran.expect("the casper put the program on a pty");
    assert!(alive(ran), "it is up while the casper holding it is");

    caller.killed();
    let went = gone_within(ran, NOTICES_WITHIN);

    caller.cleared();
    assert!(
        went,
        "a program on a screen must not outlive the casper that started it"
    );
}
