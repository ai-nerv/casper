//! Bubblewrap's post-namespace syscall filter, passed through an anonymous descriptor.

use std::io::{Seek, Write};
use std::os::fd::OwnedFd;

pub(super) fn descriptor(filter: &seccompiler::BpfProgram) -> std::io::Result<OwnedFd> {
    let fd = rustix::fs::memfd_create("casper-seccomp", rustix::fs::MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    for instruction in filter {
        file.write_all(&instruction.code.to_ne_bytes())?;
        file.write_all(&[instruction.jt, instruction.jf])?;
        file.write_all(&instruction.k.to_ne_bytes())?;
    }
    file.rewind()?;
    Ok(file.into())
}

/// The jail's seccomp filters, armed on real processes.
#[cfg(test)]
mod tests {
    use super::super::{absent, filter};
    use std::process::Command;

    const PROBE: &str = "CASPER_SYSCALL_PROBE";

    /// Run this test binary's [`probe`] under the filters for a reach grant.
    fn probed(unix: bool) -> std::process::ExitStatus {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", "jail::filter::tests::probe", "--nocapture"])
            .env(PROBE, if unix { "unix" } else { "none" });
        crate::tied::confine(&mut command, vec![filter(true, unix), absent()]);
        command.status().expect("probe runs")
    }

    #[test]
    fn probe() {
        let Ok(expected) = std::env::var(PROBE) else {
            return;
        };
        use rustix::net::{AddressFamily, SocketFlags, SocketType};
        let unix = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None);
        if expected == "unix" {
            assert!(unix.is_ok(), "{unix:?}");
        } else {
            assert_eq!(unix.err(), Some(rustix::io::Errno::PERM));
        }
        let pair = SocketFlags::CLOEXEC;
        rustix::net::socketpair(AddressFamily::UNIX, SocketType::STREAM, pair, None)
            .expect("a socketpair stays allowed");
        std::net::UdpSocket::bind("127.0.0.1:0").expect("the network stays open");
        std::net::TcpListener::bind("127.0.0.1:0").expect("the network stays open");
        let started = std::thread::spawn(|| 7).join().ok();
        assert_eq!(started, Some(7), "threads start");
    }

    #[test]
    fn reach_without_bubblewrap_opens_the_network_and_never_a_unix_socket() {
        assert!(probed(false).success());
    }

    #[test]
    fn reach_under_bubblewrap_leaves_unix_sockets_to_the_mount_namespace() {
        assert!(probed(true).success());
    }

    /// Whether `program args` succeeds, armed with the jail's filters or not.
    fn runs(armed: bool, program: &str, args: &[&str]) -> bool {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if armed {
            crate::tied::confine(&mut command, vec![filter(true, true), absent()]);
        }
        command.status().is_ok_and(|status| status.success())
    }

    #[test]
    fn a_new_user_namespace_and_an_unusual_persona_are_refused() {
        for (program, args) in [
            ("unshare", &["-U", "true"][..]),
            ("setarch", &["-R", "true"]),
        ] {
            if !runs(false, program, args) {
                eprintln!("NOT VERIFIED: {program} {args:?} fails here even unjailed");
                continue;
            }
            assert!(
                !runs(true, program, args),
                "{program} {args:?} ran in the jail"
            );
        }
        let plain = &["linux64", "true"][..];
        assert!(runs(true, "setarch", plain) || !runs(false, "setarch", plain));
    }

    #[test]
    fn clone3_is_absent_so_threads_fall_back_and_x32_is_absent_too() {
        let probe = "import ctypes; c = ctypes.CDLL(None, use_errno=True); \
                     assert c.syscall(435, 0, 0) == -1 and ctypes.get_errno() == 38; \
                     assert c.syscall(0x40000000 | 39) == -1 and ctypes.get_errno() == 38; \
                     import threading; t = threading.Thread(target=print); t.start(); t.join()";
        if !runs(false, "python3", &["-c", "pass"]) {
            eprintln!("NOT VERIFIED: no python3 to make a raw syscall with");
            return;
        }
        assert!(runs(true, "python3", &["-c", probe]));
        assert!(runs(true, "sh", &["-c", "true & wait"]));
    }

    #[test]
    fn every_x32_call_is_absent_on_x86_64() {
        let program = absent();
        let x32 = program
            .iter()
            .any(|at| at.code == 0x35 && at.k == 0x4000_0000);
        assert_eq!(x32, cfg!(target_arch = "x86_64"), "{program:?}");
    }
}
