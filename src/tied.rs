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
//! by nobody. What runs down there is three raw syscalls and a comparison, and nothing else may
//! be added to it.
//!
//! **The great-grandchild is where the chain used to end.** A program casper starts can start
//! anything, and each `fork` clears the signal again: the `shell` tool's wrapper died with its
//! casper and left the command it had forked for running under init. Nothing casper can set on
//! the wrapper reaches past it, so what casper hands over instead is a *handle* — the wrapper
//! leads a process group of its own, and a program that wants its whole subtree to go can aim
//! one signal at that group when its own death signal arrives. `config/tools.lua` does exactly
//! that; a tool that does not is no worse off than before.

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

/// Lead a process group of this program's own, from inside the child.
///
/// Async-signal-safe on the same terms as [`arm`]: one raw syscall, nothing else.
///
/// It is what makes "kill the command" mean "kill the command and what it started". A program
/// that forks — a shell above all — leaves children the death signal never reaches, and the only
/// address that covers all of them at once is a process group. This one is a group casper is not
/// in, which is the half that matters as much: a program aiming a signal at its own group here
/// cannot reach casper, the magi above it, or the person's session, because none of them are in
/// it. Without this the same signal would go to whatever group casper was spawned into.
fn alone() -> std::io::Result<()> {
    rustix::process::setpgid(None, None)?;
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
// SAFETY: the closure is `alone` and `arm` and a `Pid` copied into it: three raw syscalls and a
// comparison, which is all `pre_exec` permits between the fork and the exec of a process that
// has other threads. See the module documentation.
#[allow(unsafe_code)]
pub fn running(command: &mut std::process::Command) {
    let casper = rustix::process::getpid();
    unsafe {
        command.pre_exec(move || {
            alone()?;
            arm(casper)
        })
    };
}

/// The same, for a program being put on a pty.
///
/// Its own hangup already covers most of this: casper dying closes the master, the kernel sends
/// `SIGHUP` to the session on the far end, and a program that takes it the ordinary way is gone.
/// A program that ignores `SIGHUP` is not, and one was left running behind a killed casper to
/// prove it. The two are not alternatives — the hangup is what lets a program end the way it
/// would in any terminal, and this is the floor underneath when it will not.
///
/// No [`alone`] here, and it would be a mistake to add one: `pty_process` has already made this
/// a session leader, and `setpgid` on a session leader is `EPERM` — which `arm` reports as a
/// refusal, so every screen would fail to open. A session is a stronger form of the same thing
/// anyway, and the hangup that comes with it is what a group has to be signalled by hand.
// SAFETY: as for [`running`]. `pty_process` composes this after its own `setsid` and `ioctl`,
// both of which are equally safe down there, and the death signal survives both.
#[allow(unsafe_code)]
#[must_use]
pub fn on_a_screen(command: pty_process::blocking::Command) -> pty_process::blocking::Command {
    let casper = rustix::process::getpid();
    unsafe { command.pre_exec(move || arm(casper)) }
}
