//! The address families a jailed command may open a socket in, and bubblewrap's post-namespace
//! syscall filters, passed through an anonymous descriptor.

use std::io::{Seek, Write};
use std::os::fd::OwnedFd;

/// The families every jailed `socket` may open: the network, and routing netlink for the C
/// library's address lookups. Netlink is narrowed to `NETLINK_ROUTE` by [`families`].
const NETWORK: &[i32] = &[libc::AF_INET, libc::AF_INET6, libc::AF_NETLINK];

pub(super) fn syscalls(reach: bool, native_control: bool) -> seccompiler::BpfProgram {
    use super::{FORBIDDEN, PERSONAS, Rules, UNREACHED, built, rule};
    use seccompiler::SeccompCmpOp as Op;
    let newuser = libc::CLONE_NEWUSER as u64;
    let personas: Vec<_> = PERSONAS.iter().map(|p| (0, Op::Ne, *p)).collect();
    let mut rules: Rules = vec![
        (
            libc::SYS_clone,
            vec![rule(&[(0, Op::MaskedEq(newuser), newuser)])],
        ),
        (libc::SYS_personality, vec![rule(&personas)]),
    ];
    rules.extend(FORBIDDEN.iter().map(|nr| (*nr, Vec::new())));
    if !reach {
        rules.extend(UNREACHED.iter().map(|nr| {
            let conditions = if native_control && *nr == libc::SYS_sendmsg {
                vec![rule(&[(0, Op::Ne, 0)])]
            } else {
                Vec::new()
            };
            (*nr, conditions)
        }));
    }
    built(rules, libc::EPERM)
}

/// The allowlist on `socket` and `socketpair`, answering `EAFNOSUPPORT` outside it: [`NETWORK`],
/// netlink only as `NETLINK_ROUTE`, Unix sockets only when `unix`, and a `socketpair` only of
/// Unix sockets. `unix` is the bubblewrap path, whose mount namespace hides the runtime directory.
pub(super) fn families(unix: bool) -> seccompiler::BpfProgram {
    use seccompiler::SeccompCmpOp as Op;
    let family = |op: Op, family: i32| (0, op, u64::from(family.unsigned_abs()));
    let local = unix.then_some(libc::AF_UNIX);
    let other: Vec<_> = NETWORK
        .iter()
        .copied()
        .chain(local)
        .map(|allowed| family(Op::Ne, allowed))
        .collect();
    let routing = u64::from(libc::NETLINK_ROUTE.unsigned_abs());
    let netlink = [family(Op::Eq, libc::AF_NETLINK), (2, Op::Ne, routing)];
    let rules = vec![
        (
            libc::SYS_socket,
            vec![super::rule(&other), super::rule(&netlink)],
        ),
        (
            libc::SYS_socketpair,
            vec![super::rule(&[family(Op::Ne, libc::AF_UNIX)])],
        ),
    ];
    super::built(rules, libc::EAFNOSUPPORT)
}

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
    use super::families;
    use rustix::io::Errno;
    use rustix::net::{AddressFamily as Family, SocketFlags, SocketType as Kind, netlink};
    use std::process::Command;

    const PROBE: &str = "CASPER_SYSCALL_PROBE";

    /// Whether this kernel opens an IPv6 socket at all, asked unfiltered.
    const SIX: &str = "CASPER_SYSCALL_PROBE_INET6";

    /// Run this test binary's [`probe`] under the filters for a reach grant.
    fn probed(unix: bool) -> std::process::ExitStatus {
        let six = rustix::net::socket(Family::INET6, Kind::STREAM, None).is_ok();
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", "jail::filter::tests::probe", "--nocapture"])
            .env(PROBE, if unix { "unix" } else { "none" })
            .env(SIX, if six { "1" } else { "" })
            .stdout(std::process::Stdio::null());
        let filters = vec![filter(true), families(unix), absent()];
        crate::tied::confine(&mut command, filters);
        command.status().expect("probe runs")
    }

    #[test]
    fn probe() {
        let Ok(expected) = std::env::var(PROBE) else {
            return;
        };
        let opened = |family, kind, protocol| rustix::net::socket(family, kind, protocol).err();
        let refused = Some(Errno::AFNOSUPPORT);
        let unix = opened(Family::UNIX, Kind::STREAM, None);
        assert_eq!(unix, (expected != "unix").then_some(Errno::AFNOSUPPORT));
        assert_eq!(opened(Family::INET, Kind::STREAM, None), None);
        assert_eq!(opened(Family::INET, Kind::DGRAM, None), None);
        if std::env::var_os(SIX).is_some_and(|six| !six.is_empty()) {
            assert_eq!(opened(Family::INET6, Kind::STREAM, None), None);
        }
        assert_eq!(opened(Family::NETLINK, Kind::RAW, None), None, "routing");
        for protocol in [netlink::KOBJECT_UEVENT, netlink::AUDIT, netlink::SOCK_DIAG] {
            let other = opened(Family::NETLINK, Kind::RAW, Some(protocol));
            assert_eq!(other, refused, "{protocol:?}");
        }
        for (family, kind) in [
            (Family::PACKET, Kind::DGRAM),
            (Family::from_raw(libc::AF_ALG as u16), Kind::SEQPACKET),
            (Family::VSOCK, Kind::STREAM),
            (Family::BLUETOOTH, Kind::STREAM),
            (Family::TIPC, Kind::RDM),
        ] {
            assert_eq!(opened(family, kind, None), refused, "{family:?}");
        }
        let pair =
            |family| rustix::net::socketpair(family, Kind::STREAM, SocketFlags::CLOEXEC, None);
        pair(Family::UNIX).expect("a socketpair stays allowed");
        assert_eq!(pair(Family::INET).err(), refused);
        std::net::UdpSocket::bind("127.0.0.1:0").expect("the network stays open");
        std::net::TcpListener::bind("127.0.0.1:0").expect("the network stays open");
        let started = std::thread::spawn(|| 7).join().ok();
        assert_eq!(started, Some(7), "threads start");
    }

    #[test]
    fn reach_without_bubblewrap_opens_only_the_network_families_and_never_a_unix_socket() {
        assert!(probed(false).success());
    }

    #[test]
    fn reach_under_bubblewrap_opens_only_the_network_families_and_unix_sockets() {
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
            crate::tied::confine(&mut command, vec![filter(true), families(true), absent()]);
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
