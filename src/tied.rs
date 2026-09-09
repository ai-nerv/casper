//! Programs casper starts die with casper. `PR_SET_PDEATHSIG` is cleared across `fork`, so each
//! child re-arms it in the `pre_exec` window, then re-reads its parent to catch a casper already
//! gone. The signal fires when the *forking thread* exits, so spawn from the main one.
//! [`running`] also gives the child its own process group, so one signal reaches what it forks;
//! [`on_a_screen`] must not — `pty_process` made it a session leader, and `setpgid` on one fails.

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
