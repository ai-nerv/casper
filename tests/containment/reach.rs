use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::Path;

/// A runtime directory with a family socket and a session bus in it, an abstract socket named
/// like an X server's, and the probe that tries all three from inside the jail.
pub(super) fn prepare(dir: &Path) -> Vec<UnixListener> {
    use std::os::unix::fs::DirBuilderExt;
    let runtime = dir.join("runtime");
    let family = runtime.join("magi");
    for path in [runtime.clone(), family.clone()] {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("runtime fixture");
    }
    let x11 = format!(
        "/tmp/.X11-unix/casper-{}",
        dir.file_name().expect("name").to_string_lossy()
    );
    let held = vec![
        UnixListener::bind(family.join("host.sock")).expect("family socket"),
        UnixListener::bind(runtime.join("bus")).expect("session bus"),
        UnixListener::bind_addr(&SocketAddr::from_abstract_name(&x11).expect("abstract name"))
            .expect("abstract socket"),
    ];
    let targets = format!(
        "{}\n{}\n{x11}\n",
        family.join("host.sock").display(),
        runtime.join("bus").display()
    );
    std::fs::write(dir.join("work/reach-targets"), targets).expect("targets");
    std::fs::copy(
        std::env::current_exe().expect("test executable"),
        dir.join("work/reach-probe"),
    )
    .expect("reach probe executable");
    held
}

/// Give this side a controlling terminal, so a jailed command reaching `/dev/tty` would reach it.
/// The terminal is held until the process exits: closing it early would hang this session up.
pub(super) fn own_a_terminal() {
    rustix::process::setsid().expect("a session of its own");
    let (pty, pts) = pty_process::blocking::open().expect("terminal");
    rustix::process::ioctl_tiocsctty(&pts).expect("controlling terminal");
    assert!(
        std::fs::File::open("/dev/tty").is_ok(),
        "the fixture has no controlling terminal to protect"
    );
    std::mem::forget(pty);
}

#[test]
fn probe() {
    if std::env::current_exe().expect("executable").file_name()
        != Some(std::ffi::OsStr::new("reach-probe"))
    {
        return;
    }
    let targets = std::fs::read_to_string("reach-targets").expect("targets");
    let mut lines = targets.lines();
    for path in lines.by_ref().take(2) {
        assert!(
            UnixStream::connect(path).is_err(),
            "jailed command reached {path}"
        );
    }
    let x11 = lines.next().expect("abstract name");
    let addr = SocketAddr::from_abstract_name(x11).expect("abstract name");
    assert!(
        UnixStream::connect_addr(&addr).is_err(),
        "jailed command reached the abstract socket {x11}"
    );
    assert!(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_err(),
        "jailed command opened the controlling terminal"
    );
}
