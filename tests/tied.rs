//! Whether the kernel takes a casper, and the program it was running, with the process that
//! started it. Against the real binary: `PR_SET_PDEATHSIG` is a property of a live process and
//! its parent. The caller is *killed* rather than exiting, because a caller with a way out can
//! end its children on the way. The signal is cleared across `fork`, so the command under casper
//! and the command under that are checked separately from casper itself.

use casper::scratch::Scratch;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const NOTICES_WITHIN: Duration = Duration::from_secs(10);

const CASPER: &str = env!("CARGO_BIN_EXE_casper");

/// A parent that starts one `casper run` and then does nothing at all. A shell rather than this
/// process, because the parent has to be something the test can kill outright.
struct Caller {
    shell: Child,
    /// Zero until the shell has said which casper it started. See [`Caller::spawning`].
    served: u32,
    ran: Option<u32>,
    /// The program the person actually asked for, below whatever wrapper is between.
    leaf: Option<u32>,
    /// Everything else the caller started, read while it was all still up. See [`below`].
    under: Vec<u32>,
    _dir: Scratch,
}

impl Caller {
    /// Start a shell that starts a casper on a call that will still be running. A regular file
    /// for stdin, so it is at end of file before the command starts and no pipe is left for a
    /// dying caller to close. The `sleep` is looked for from casper rather than from its first
    /// child, a `cat` with nothing under it by the time anybody looks.
    fn starting(name: &str) -> Self {
        let mut caller = Self::spawning(
            name,
            r#"{"tool":"shell","args":{"command":"sleep 30"}}"#,
            |frame| format!("{CASPER} run <{}", frame.display()),
        );
        caller.leaf = the_command_under(caller.served, "sleep");
        caller.ran = child_of(caller.served);
        caller
    }

    /// The same, for a program on a pty. It ignores `SIGHUP` on purpose: any other would go when
    /// the master closes and the session is hung up, and pass against a casper that had armed
    /// nothing. A pipe for stdin rather than a file, because on end of file the frame loop is
    /// over and the casper exits on its own rather than being taken.
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

    /// The shell, the casper under it, and a fixture of this test's own. The frame is
    /// newline-terminated, because a surface reads lines and would wait for the rest of an
    /// unterminated one. All three XDG directories are redirected into the fixture, so casper
    /// reads the declarations this repository ships rather than the ones on this machine.
    fn spawning(name: &str, frame: &str, running: impl Fn(&Path) -> String) -> Self {
        let dir = Scratch::new("casper-tied", name);
        let call = dir.join("call.json");
        std::fs::write(&call, format!("{frame}\n")).expect("wrote");

        let pids = dir.join("pid");
        let script = format!(
            "{casper} >/dev/null 2>&1 & echo $! > {pids}; wait",
            casper = running(&call),
            pids = pids.display(),
        );
        let config = dir.join("config/casper");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("config/tools.lua"),
            config.join("tools.lua"),
        )
        .expect("the shipped declarations");

        let shell = Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_RUNTIME_DIR", &*dir)
            .env("XDG_DATA_HOME", dir.join("data"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the caller");
        // Owned before anything else can panic: `Child` has no `Drop` that kills, so everything
        // after this line unwinds into the guard below.
        let mut caller = Self {
            shell,
            served: 0,
            ran: None,
            leaf: None,
            under: Vec::new(),
            _dir: dir,
        };
        caller.served = read_pid(&pids).expect("the caller said which casper it started");
        caller.ran = running_under(caller.served);
        caller.under = below(caller.shell.id());
        caller
    }

    /// End the caller the way a crash would: with nothing running inside it.
    fn killed(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }
}

/// Leave nothing running and nothing on disk, whatever the assertions did. A guard, because every
/// `expect` and `assert!` above unwinds past a call placed before them.
impl Drop for Caller {
    fn drop(&mut self) {
        // Before the shell, not after: a test may already have killed it, and reading `/proc`
        // for a pid that has been reaped asks about whoever holds it now.
        for pid in [Some(self.served), self.ran, self.leaf]
            .into_iter()
            .flatten()
            .chain(self.under.iter().copied())
        {
            end(pid);
        }
        self.killed();
    }
}

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

/// Every child of `pid` right now, read out of `/proc`: it was not spawned by this process.
fn children_of(pid: u32) -> Vec<u32> {
    let Ok(threads) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for thread in threads.flatten() {
        if let Ok(listed) = std::fs::read_to_string(thread.path().join("children")) {
            found.extend(
                listed
                    .split_whitespace()
                    .filter_map(|one| one.parse::<u32>().ok()),
            );
        }
    }
    found
}

fn child_of(pid: u32) -> Option<u32> {
    children_of(pid).into_iter().next()
}

/// Everything running below `pid`, however deep, not counting `pid` itself. Must be called while
/// everything is still up: once the shell is gone its children are reparented to init and `/proc`
/// no longer relates them to it.
fn below(pid: u32) -> Vec<u32> {
    let mut walked = vec![pid];
    let mut at = 0;
    while at < walked.len() {
        let next = children_of(walked[at]);
        walked.extend(next);
        at += 1;
    }
    walked.remove(0);
    walked
}

fn running_under(pid: u32) -> Option<u32> {
    for _ in 0..250 {
        if let Some(child) = child_of(pid) {
            return Some(child);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// The process called `comm` somewhere under `pid`, once it is there. By name and not by depth,
/// because the depth is the thing under test: the wrapper always died with its casper.
fn the_command_under(pid: u32, comm: &str) -> Option<u32> {
    for _ in 0..250 {
        let mut at = pid;
        while let Some(next) = child_of(at) {
            let named = std::fs::read_to_string(format!("/proc/{next}/comm")).unwrap_or_default();
            if named.trim() == comm {
                return Some(next);
            }
            at = next;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Whether a process exists and is not merely a corpse waiting to be reaped. The state field
/// rather than the directory's existence, because a zombie is the expected shape of "gone" here.
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|stat| stat.split_whitespace().nth(2) != Some("Z"))
}

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

/// Kill one process outright. Never pid 0: `kill -9 0` means this whole process group, which from
/// here is the test runner. A [`Caller`] carries a zero between the spawn and the pid being read.
fn end(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        // Already gone is the passing case, and its complaint reads like a failure.
        .stderr(Stdio::null())
        .status();
}

/// The guarantee at all three depths: casper, the command it runs, and the command under the
/// shell `shell` wraps that in. Every `fork` on the way down clears the death signal again.
#[test]
fn a_call_does_not_outlive_the_process_that_asked_for_it() {
    let mut caller = Caller::starting("killed");
    let served = caller.served;
    let ran = caller.ran.expect("the casper started what it was given");
    let leaf = caller
        .leaf
        .expect("the shell started the command it was given");
    assert!(
        alive(served),
        "it is up while the caller that started it is"
    );

    caller.killed();
    let went = gone_within(served, NOTICES_WITHIN);
    let ran_went = gone_within(ran, NOTICES_WITHIN);
    let leaf_went = gone_within(leaf, NOTICES_WITHIN);

    assert!(
        went,
        "a casper must not outlive the process that started it"
    );
    assert!(
        ran_went,
        "a command must not outlive the casper that started it"
    );
    assert!(
        leaf_went,
        "nor may the command a tool wrapped in a shell: {leaf} is still running"
    );
}

/// The same on a pty, with a program that refuses the hangup so only the death signal can end it.
#[test]
fn a_program_on_a_screen_does_not_outlive_it_either() {
    let mut caller = Caller::on_a_screen("screened");
    let ran = caller.ran.expect("the casper put the program on a pty");
    assert!(alive(ran), "it is up while the casper holding it is");

    caller.killed();
    let went = gone_within(ran, NOTICES_WITHIN);

    assert!(
        went,
        "a program on a screen must not outlive the casper that started it"
    );
}
