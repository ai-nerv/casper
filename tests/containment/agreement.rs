use rustix::net::{AddressFamily as Family, SocketFlags, SocketType as Kind, netlink};
use std::io::Write as _;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};

/// Each probe and whether it is allowed under bubblewrap, then under Landlock, with reach on.
pub(super) const EXPECTED: &[(&str, bool, bool)] = &[
    ("read-ssh-key", false, false),
    ("read-credentials", false, false),
    ("read-provider-key", false, false),
    ("write-outside", false, false),
    ("write-home-dotfile", false, false),
    ("write-tools-config", false, false),
    ("write-coordinator-config", false, false),
    ("read-system", true, true),
    ("write-workspace", true, true),
    ("write-git-directory", true, true),
    ("write-tmp", true, true),
    ("write-project-config", false, true),
    ("write-memory-config", false, true),
    ("write-git-config", false, true),
    ("write-git-hook", false, true),
    ("tcp-loopback", true, true),
    ("udp-loopback", true, true),
    ("netlink-route", true, true),
    ("netlink-audit", false, false),
    ("packet-socket", false, false),
    ("unix-socket", true, false),
    ("unix-socketpair", true, true),
    ("unix-host-socket", false, false),
    ("abstract-socket", false, false),
    ("controlling-terminal", false, false),
    ("threads", true, true),
];

/// The probes the two backends answer differently, each for a reason the README names: Landlock
/// opens no Unix socket, having no mount namespace to hide the runtime directory, and cannot seal
/// a file beneath a writable root.
pub(super) const DIFFERENT: &[&str] = &[
    "write-project-config",
    "write-memory-config",
    "write-git-config",
    "write-git-hook",
    "unix-socket",
];

fn readable(path: &str) -> bool {
    std::fs::read(path).is_ok()
}

/// Whether `path` can be written: made when `new`, otherwise only as the file already there, so
/// a path bubblewrap does not mount is not mistaken for one written through to the host.
fn writable(path: impl AsRef<std::path::Path>, new: bool) -> bool {
    std::fs::OpenOptions::new()
        .append(true)
        .create(new)
        .open(path)
        .and_then(|mut file| file.write_all(b"x"))
        .is_ok()
}

fn opens(family: Family, kind: Kind, protocol: Option<rustix::net::Protocol>) -> bool {
    rustix::net::socket(family, kind, protocol).is_ok()
}

/// One verdict, as the probe sees it from inside the jail.
fn verdict(name: &str) -> bool {
    let targets = std::fs::read_to_string("reach-targets").expect("targets");
    let target = |nth: usize| targets.lines().nth(nth).unwrap_or_default().to_owned();
    let [dotfile, _, tools, coordinator] = super::persistence::outside(std::path::Path::new(".."));
    match name {
        "read-ssh-key" => readable("../home/.ssh/id"),
        "read-credentials" => readable("../data/magi/credentials.json"),
        "read-provider-key" => std::env::var_os("OPENROUTER_API_KEY").is_some(),
        "write-outside" => writable("../outside", false),
        "write-home-dotfile" => writable(dotfile, false),
        "write-tools-config" => writable(tools, false),
        "write-coordinator-config" => writable(coordinator, false),
        "read-system" => readable("/etc/hosts"),
        "write-workspace" => writable("agreement-file", true),
        "write-git-directory" => writable(".git/agreement-file", true),
        "write-tmp" => writable(std::env::temp_dir().join("agreement"), true),
        "write-project-config" => writable(".magi.lua", false),
        "write-memory-config" => writable(".balthasar.lua", false),
        "write-git-config" => writable(".git/config", false),
        "write-git-hook" => writable(".git/hooks/pre-commit", false),
        "tcp-loopback" => std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|held| std::net::TcpStream::connect(held.local_addr()?))
            .is_ok(),
        "udp-loopback" => std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.send_to(b"x", "127.0.0.1:9"))
            .is_ok(),
        "netlink-route" => opens(Family::NETLINK, Kind::RAW, None),
        "netlink-audit" => opens(Family::NETLINK, Kind::RAW, Some(netlink::AUDIT)),
        "packet-socket" => opens(Family::PACKET, Kind::DGRAM, None),
        "unix-socket" => opens(Family::UNIX, Kind::STREAM, None),
        "unix-socketpair" => {
            rustix::net::socketpair(Family::UNIX, Kind::STREAM, SocketFlags::CLOEXEC, None).is_ok()
        }
        "unix-host-socket" => UnixStream::connect(target(0)).is_ok(),
        "abstract-socket" => SocketAddr::from_abstract_name(target(2))
            .and_then(|at| UnixStream::connect_addr(&at))
            .is_ok(),
        "controlling-terminal" => std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok(),
        "threads" => std::thread::spawn(|| 7).join().ok() == Some(7),
        other => panic!("no probe named {other}"),
    }
}

#[test]
fn probe() {
    if std::env::current_exe().expect("executable").file_name()
        != Some(std::ffi::OsStr::new("agreement-probe"))
    {
        return;
    }
    let lines: String = EXPECTED
        .iter()
        .map(|(name, _, _)| format!("{name} {}\n", verdict(name)))
        .collect();
    std::fs::write("verdicts", lines).expect("verdicts");
}

/// The verdicts a probe left, by name.
pub(super) fn read(said: &str) -> std::collections::BTreeMap<String, bool> {
    said.lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, allowed)| (name.to_owned(), allowed == "true"))
        .collect()
}
