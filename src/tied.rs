//! The crate's one fork/exec window: what a child gets between fork and exec lives here, so the
//! `unsafe` of `pre_exec` is audited in one place — a death tie, and the jail's walls.
//!
//! `PR_SET_PDEATHSIG` is cleared across `fork`, so each child re-arms it in the window, then
//! re-reads its parent to catch a casper already gone; the signal fires when the *forking thread*
//! exits, so spawn from the main one. [`running`] also gives the child its own process group;
//! [`on_a_screen`] must not — `pty_process` made it a session leader. [`confine`] and [`restrict`]
//! arm the seccomp filter and Landlock ruleset [`crate::jail`] builds as data.

use std::os::unix::process::CommandExt as _;

fn arm(casper: rustix::process::Pid) -> std::io::Result<()> {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
    if rustix::process::getppid() != Some(casper) {
        return Err(rustix::io::Errno::SRCH.into());
    }
    Ok(())
}

fn alone() -> std::io::Result<()> {
    rustix::process::setpgid(None, None)?;
    Ok(())
}

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

// SAFETY: as for [`running`]. `pty_process` composes this after its own `setsid` and `ioctl`,
// both of which are equally safe down there, and the death signal survives both.
#[allow(unsafe_code)]
#[must_use]
pub fn on_a_screen(command: pty_process::blocking::Command) -> pty_process::blocking::Command {
    let casper = rustix::process::getpid();
    unsafe { command.pre_exec(move || arm(casper)) }
}

// SAFETY: the closure only calls `apply_filter` on a filter built and moved in before the fork —
// one `prctl` pair, nothing allocated in the child.
#[allow(unsafe_code)]
pub fn confine(command: &mut std::process::Command, filter: seccompiler::BpfProgram) {
    unsafe {
        command.pre_exec(move || {
            seccompiler::apply_filter(&filter)
                .map_err(|why| std::io::Error::other(format!("seccomp: {why}")))
        });
    }
}

// SAFETY: the closure clones the ruleset's descriptor and calls `restrict_self` — a `prctl` and one
// Landlock syscall, no allocation, on a ruleset built and moved in before the fork.
#[allow(unsafe_code)]
pub fn restrict(command: &mut std::process::Command, ruleset: landlock::RulesetCreated) {
    unsafe {
        command.pre_exec(move || {
            let status = ruleset
                .try_clone()
                .map_err(|why| std::io::Error::other(format!("landlock: {why}")))?
                .restrict_self()
                .map_err(|why| std::io::Error::other(format!("landlock: {why}")))?;
            if status.ruleset == landlock::RulesetStatus::NotEnforced {
                return Err(std::io::Error::other("landlock: not enforced"));
            }
            Ok(())
        });
    }
}
