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

// SAFETY: sealing and inheriting descriptors, session creation, and parent-death setup use raw
// syscalls on prebuilt data, without allocating between fork and exec.
#[allow(unsafe_code)]
pub fn running(command: &mut std::process::Command, descriptors: Vec<std::os::fd::OwnedFd>) {
    let casper = rustix::process::getpid();
    unsafe {
        command.pre_exec(move || {
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            inherit(&descriptors)?;
            alone()?;
            arm(casper)
        })
    };
}

// SAFETY: the forked supervisor uses fixed storage and raw syscalls, closes inherited descriptors,
// and exits without running destructors; only the server continues through the existing exec hooks.
#[allow(unsafe_code)]
pub(crate) fn supervised(
    command: &mut std::process::Command,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd as _;
    let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
    let owner = rustix::process::pidfd_open(
        rustix::process::getpid(),
        rustix::process::PidfdFlags::empty(),
    )?;
    if reader.as_raw_fd() < 3 || writer.as_raw_fd() < 3 || owner.as_raw_fd() < 3 {
        return Err(std::io::Error::other(
            "MCP supervision requires open standard descriptors",
        ));
    }
    let writer_fd = writer.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            let mut previous: libc::sigset_t = std::mem::zeroed();
            libc::sigfillset(&mut blocked);
            if libc::sigprocmask(libc::SIG_SETMASK, &blocked, &mut previous) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            if libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) != 0
                || libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            alone()?;
            if libc::syscall(libc::SYS_close_range, u32::MAX, u32::MAX, 0u32) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let children = libc::open(
                c"/proc/thread-self/children".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            );
            if children < 3 {
                return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
            }
            let supervisor = rustix::process::getpid();
            let server = libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0) as libc::pid_t;
            if server < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if server == 0 {
                libc::close(children);
                libc::close(writer_fd);
                arm(supervisor)?;
                if libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                return Ok(());
            }
            libc::dup2(reader.as_raw_fd(), 0);
            libc::dup2(owner.as_raw_fd(), 1);
            libc::dup2(children, 2);
            libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
            let mut stopping = false;
            let mut result = 125;
            let mut descriptors = [
                libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: 1,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let mut bytes = [0u8; 4096];
            let mut scanning = false;
            loop {
                for _ in 0..256 {
                    if scanning {
                        break;
                    }
                    let mut status = 0;
                    let reaped = libc::waitpid(-1, &mut status, libc::WNOHANG | libc::__WALL);
                    if reaped == server {
                        stopping = true;
                        result = if libc::WIFEXITED(status) {
                            libc::WEXITSTATUS(status)
                        } else {
                            128 + libc::WTERMSIG(status)
                        };
                    }
                    if reaped < 0 && *libc::__errno_location() == libc::ECHILD {
                        libc::_exit(result);
                    }
                    if reaped <= 0 {
                        break;
                    }
                }
                if !stopping {
                    let ready = libc::poll(descriptors.as_mut_ptr(), 2, 10);
                    stopping = ready != 0;
                }
                if stopping {
                    scanning = true;
                    let mut pid = 0i32;
                    for _ in 0..16 {
                        let count = libc::read(2, bytes.as_mut_ptr().cast(), bytes.len());
                        if count <= 0 {
                            libc::lseek(2, 0, libc::SEEK_SET);
                            scanning = false;
                            pid = 0;
                            break;
                        }
                        for byte in bytes.iter().take(count as usize) {
                            if byte.is_ascii_digit() {
                                pid = pid
                                    .saturating_mul(10)
                                    .saturating_add(i32::from(*byte - b'0'));
                            } else if pid > 0 {
                                libc::kill(pid, libc::SIGKILL);
                                pid = 0;
                            }
                        }
                    }
                    // Complete the current PID before the next reap can reuse it.
                    for _ in 0..11 {
                        if pid == 0 {
                            break;
                        }
                        let mut byte = 0u8;
                        if libc::read(2, (&raw mut byte).cast(), 1) != 1 {
                            libc::lseek(2, 0, libc::SEEK_SET);
                            scanning = false;
                            break;
                        }
                        if byte.is_ascii_digit() {
                            pid = pid
                                .saturating_mul(10)
                                .saturating_add(i32::from(byte - b'0'));
                        } else {
                            libc::kill(pid, libc::SIGKILL);
                            pid = 0;
                        }
                    }
                    let delay = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 5_000_000,
                    };
                    libc::nanosleep(&delay, std::ptr::null_mut());
                }
            }
        });
    }
    Ok(writer.into())
}

// SAFETY: the server retains its supervisor death tie, seals all but prebuilt descriptors with a
// raw syscall and establishes its own session before exec.
#[allow(unsafe_code)]
pub(crate) fn inside_supervisor(
    command: &mut std::process::Command,
    descriptors: Vec<std::os::fd::OwnedFd>,
) {
    unsafe {
        command.pre_exec(move || {
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            inherit(&descriptors)?;
            alone()
        });
    }
}

// SAFETY: descriptor sealing, enforcement and parent-death hooks use prebuilt data and raw
// syscalls after PTY setup.
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
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn instance(pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|text| {
                text.rsplit_once(") ")?
                    .1
                    .split_whitespace()
                    .nth(19)
                    .map(str::to_owned)
            })
    }

    fn descendant(root: &std::path::Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(text) = std::fs::read_to_string(root.join("descendant"))
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "descendant startup");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn finished(child: &mut std::process::Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().expect("state") {
                return status;
            }
            assert!(Instant::now() < deadline, "bounded subtree cleanup");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn supervisor_preserves_stdio_and_the_real_server_exit_status() {
        let mut command = Command::new("sh");
        command.args(["-c", "read value; printf 'server:%s' \"$value\"; exit 7"]);
        let lifetime = supervised(&mut command).expect("supervisor");
        inside_supervisor(&mut command, Vec::new());
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        use std::io::Write as _;
        child
            .stdin
            .take()
            .expect("input")
            .write_all(b"literal-value\n")
            .expect("write");
        let output = child.wait_with_output().expect("wait");
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"server:literal-value");
        drop(lifetime);
    }

    #[test]
    fn supervisor_reaps_a_descendant_that_creates_another_session() {
        let root = crate::scratch::Scratch::new("mcp-supervisor", "session");
        let mut command = Command::new("sh");
        command.current_dir(&*root).args([
            "-c",
            "setsid sh -c 'echo $$ > descendant; exec sleep 30' & wait",
        ]);
        let lifetime = supervised(&mut command).expect("supervisor");
        inside_supervisor(&mut command, Vec::new());
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = descendant(&root);
        let before = instance(pid).expect("live descendant identity");
        drop(lifetime);
        finished(&mut child);
        assert_ne!(instance(pid), Some(before));
    }

    #[test]
    fn supervisor_reaps_double_forked_descendants_after_server_exit() {
        let root = crate::scratch::Scratch::new("mcp-supervisor", "double-fork");
        let mut command = Command::new("sh");
        command.current_dir(&*root).args([
            "-c",
            "(setsid sh -c 'echo $$ > descendant; exec sleep 30' & wait) & \
             while [ ! -f release ]; do sleep 0.01; done; exit 9",
        ]);
        let lifetime = supervised(&mut command).expect("supervisor");
        inside_supervisor(&mut command, Vec::new());
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = descendant(&root);
        let before = instance(pid).expect("live descendant identity");
        std::fs::write(root.join("release"), b"exit").expect("release server");
        assert_eq!(finished(&mut child).code(), Some(9));
        assert_ne!(instance(pid), Some(before));
        drop(lifetime);
    }

    #[test]
    fn supervisor_reports_exec_failure_without_hanging() {
        let root = crate::scratch::Scratch::new("mcp-supervisor", "missing-exec");
        let mut command = Command::new(root.join("missing-server"));
        let lifetime = supervised(&mut command).expect("supervisor");
        inside_supervisor(&mut command, Vec::new());
        let started = Instant::now();
        let error = command.spawn().expect_err("missing executable");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(lifetime);
    }

    #[test]
    fn a_child_keeps_only_the_descriptors_it_is_given() {
        use std::os::fd::OwnedFd;
        let (stray, _peer) = std::os::unix::net::UnixStream::pair().expect("stray socket");
        rustix::io::fcntl_setfd(&stray, rustix::io::FdFlags::empty()).expect("inheritable");
        let (given, _other) = std::os::unix::net::UnixStream::pair().expect("given socket");
        let socket = |fd: &dyn std::os::fd::AsFd| {
            format!("socket:[{}]", rustix::fs::fstat(fd).expect("stat").st_ino)
        };
        let (stray_name, given_name) = (socket(&stray), socket(&given));
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "for f in /proc/$$/fd/*; do readlink \"$f\" || :; done",
            ])
            .stdout(Stdio::piped());
        running(&mut command, vec![OwnedFd::from(given)]);
        let out = command.output().expect("child");
        let open = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            !open.contains(&stray_name),
            "a stray descriptor reached the child: {open}"
        );
        assert!(
            open.contains(&given_name),
            "the given descriptor did not: {open}"
        );
    }
}
