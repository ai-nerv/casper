//! The crate's one fork/exec window: what a child gets between fork and exec lives here, so the
//! `unsafe` of `pre_exec` is audited in one place — a death tie, and the jail's walls.
//!
//! `PR_SET_PDEATHSIG` is cleared across `fork`, so each child re-arms it in the window, then
//! re-reads its parent to catch a casper already gone; the signal fires when the *forking thread*
//! exits, so spawn from the main one. [`running`] also gives the child a session of its own, so it
//! has no controlling terminal and its group is one signal away; [`on_a_screen`] must not —
//! `pty_process` made it a session leader. [`confine`] and [`restrict`] arm the seccomp filters
//! and Landlock ruleset [`crate::jail`] builds as data.

use std::os::unix::process::CommandExt as _;

fn arm(casper: rustix::process::Pid) -> std::io::Result<()> {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
    if rustix::process::getppid() != Some(casper) {
        return Err(rustix::io::Errno::SRCH.into());
    }
    Ok(())
}

fn alone() -> std::io::Result<()> {
    rustix::process::setsid()?;
    Ok(())
}

// SAFETY: descriptor inheritance, session creation, and parent-death setup use raw syscalls
// on prebuilt data, without allocating between fork and exec.
#[allow(unsafe_code)]
pub fn running(command: &mut std::process::Command, descriptors: Vec<std::os::fd::OwnedFd>) {
    let casper = rustix::process::getpid();
    unsafe {
        command.pre_exec(move || {
            inherit(&descriptors)?;
            alone()?;
            arm(casper)
        })
    };
}

// SAFETY: enforcement and parent-death hooks use prebuilt data and raw syscalls after PTY setup.
#[allow(unsafe_code)]
#[must_use]
pub fn on_a_screen(
    command: pty_process::blocking::Command,
    filters: Vec<seccompiler::BpfProgram>,
    ruleset: Option<landlock::RulesetCreated>,
    descriptors: Vec<std::os::fd::OwnedFd>,
) -> pty_process::blocking::Command {
    let casper = rustix::process::getpid();
    unsafe {
        command.pre_exec(move || {
            inherit(&descriptors)?;
            enforce(&filters, ruleset.as_ref())?;
            arm(casper)
        })
    }
}

fn inherit(descriptors: &[std::os::fd::OwnedFd]) -> std::io::Result<()> {
    for descriptor in descriptors {
        rustix::io::fcntl_setfd(descriptor, rustix::io::FdFlags::empty())?;
    }
    Ok(())
}

fn enforce(
    filters: &[seccompiler::BpfProgram],
    ruleset: Option<&landlock::RulesetCreated>,
) -> std::io::Result<()> {
    let denied = |_| std::io::Error::from_raw_os_error(libc::EPERM);
    for filter in filters {
        seccompiler::apply_filter(filter).map_err(denied)?;
    }
    if let Some(ruleset) = ruleset {
        let status = ruleset
            .try_clone()
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EPERM))?
            .restrict_self()
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EPERM))?;
        if status.ruleset != landlock::RulesetStatus::FullyEnforced {
            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
        }
    }
    Ok(())
}

// SAFETY: the closure only calls `apply_filter` on filters built and moved in before the fork —
// one `prctl` pair each, nothing allocated in the child.
#[allow(unsafe_code)]
pub fn confine(command: &mut std::process::Command, filters: Vec<seccompiler::BpfProgram>) {
    unsafe {
        command.pre_exec(move || enforce(&filters, None));
    }
}

// SAFETY: the closure clones the ruleset's descriptor and calls `restrict_self` — a `prctl` and one
// Landlock syscall, no allocation, on a ruleset built and moved in before the fork.
#[allow(unsafe_code)]
pub fn restrict(command: &mut std::process::Command, ruleset: landlock::RulesetCreated) {
    unsafe {
        command.pre_exec(move || enforce(&[], Some(&ruleset)));
    }
}
