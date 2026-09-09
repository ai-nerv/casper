//! The far half of "casper belongs to the magi that started it": the programs casper starts.
//!
//! `PR_SET_PDEATHSIG` is cleared across `fork`. The tie `main` sets for casper itself therefore
//! stops at casper: killing a magi took casper with it and left the command casper was running
//! reparented to init, still holding whatever it held — a build, an editor, a process on the
//! other end of a socket. A tool call can run for a long time, so that is not a rounding error.
//!
//! The grandchild has to ask for the same thing on its own behalf, and the only place it can is
//! the window between the `fork` and the `exec`, which is what `pre_exec` is. That window is
//! also the reason this is the only `unsafe` in the crate: only the forking thread exists in the
//! child, so anything the others were holding — an allocator's lock above all — is held forever
//! by nobody. What runs down there is two raw syscalls and a comparison, and nothing else may
//! be added to it.
//!
//! **What this does not cover is the great-grandchild.** A shell casper starts can start
//! anything, and each `fork` clears the signal again. The chain is only as long as each link
//! chooses to make it; what casper owes is its own link.

use std::os::unix::process::CommandExt as _;

/// Ask the kernel to end this process when casper ends, from inside the child.
///
/// Async-signal-safe, because it has to be: `prctl` and `getppid` are raw syscalls and
/// `from_raw_os_error` only wraps a number. No allocation, no lock, no logging, no formatting.
///
/// The second syscall is for the same window `main`'s tie has: the signal watches only from the
/// moment it is set, so a casper that died between the fork and this line is a death nothing was
/// ever sent for. Here the check is exact rather than approximate — the pid to compare against
/// was read before the fork, so unlike `main` there is no first read that could already be
/// wrong. A mismatch refuses, which means the program is never `exec`ed at all: a call whose
/// caller is already gone should produce no process, not a tidy one.
fn arm(casper: rustix::process::Pid) -> std::io::Result<()> {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
    if rustix::process::getppid() != Some(casper) {
        return Err(rustix::io::Errno::SRCH.into());
    }
    Ok(())
}

/// Have `command` die with casper, however casper goes.
///
/// The signal is delivered when the *thread* that forked exits, not when the process does, which
/// is a Linux detail with teeth: a program started from a worker thread that then finishes would
/// be killed while casper was still running. Every spawn here is on the main thread — `run` runs
/// its command to completion in the one it was called on, and a screen is opened by the same
/// thread that then drives its frames — and a caller of this library that spawns from a thread
/// it lets go would be asking for something else.
// SAFETY: the closure is `arm` and a `Pid` copied into it: two raw syscalls and a comparison,
// which is all `pre_exec` permits between the fork and the exec of a process that has other
// threads. See the module documentation.
#[allow(unsafe_code)]
pub fn running(command: &mut std::process::Command) {
    let casper = rustix::process::getpid();
    unsafe { command.pre_exec(move || arm(casper)) };
}

/// The same, for a program being put on a pty.
///
/// Its own hangup already covers most of this: casper dying closes the master, the kernel sends
/// `SIGHUP` to the session on the far end, and a program that takes it the ordinary way is gone.
/// A program that ignores `SIGHUP` is not, and one was left running behind a killed casper to
/// prove it. The two are not alternatives — the hangup is what lets a program end the way it
/// would in any terminal, and this is the floor underneath when it will not.
// SAFETY: as for [`running`]. `pty_process` composes this after its own `setsid` and `ioctl`,
// both of which are equally safe down there, and the death signal survives both.
#[allow(unsafe_code)]
#[must_use]
pub fn on_a_screen(command: pty_process::blocking::Command) -> pty_process::blocking::Command {
    let casper = rustix::process::getpid();
    unsafe { command.pre_exec(move || arm(casper)) }
}
