use std::process::Child;
use std::time::{Duration, Instant};

pub(super) struct Owned(pub Child, Vec<Identity>);

impl Owned {
    pub(super) fn new(child: Child) -> Self {
        Self(child, Vec::new())
    }

    pub(super) fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        for identity in &self.1 {
            if identity.alive() {
                let _ = rustix::process::pidfd_send_signal(
                    &identity.descriptor,
                    rustix::process::Signal::KILL,
                );
            }
        }
        self.stop();
    }
}

struct Identity {
    pid: u32,
    descriptor: std::os::fd::OwnedFd,
}

impl Identity {
    fn read(pid: u32) -> Option<Self> {
        let (_, born) = state(pid)?;
        let process = rustix::process::Pid::from_raw(pid as i32)?;
        let descriptor =
            match rustix::process::pidfd_open(process, rustix::process::PidfdFlags::empty()) {
                Ok(descriptor) => descriptor,
                Err(rustix::io::Errno::SRCH) => return None,
                Err(error) => panic!("required descendant pidfd: {error}"),
            };
        if state(pid)?.1 != born {
            return None;
        }
        Some(Self { pid, descriptor })
    }

    fn alive(&self) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec};
        let mut descriptors = [PollFd::new(&self.descriptor, PollFlags::IN)];
        match rustix::event::poll(&mut descriptors, Some(&Timespec::default())) {
            Ok(_) => !descriptors[0]
                .revents()
                .intersects(PollFlags::IN | PollFlags::HUP),
            Err(_) => true,
        }
    }
}

fn state(pid: u32) -> Option<(String, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.first()?.to_string(), fields.get(19)?.to_string()))
}

fn descendants(pid: u32) -> Vec<Identity> {
    let mut walked = vec![pid];
    let mut at = 0;
    while at < walked.len() {
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{}/task", walked[at])) {
            for task in tasks.flatten() {
                let children =
                    std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
                for child in children
                    .split_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                {
                    if !walked.contains(&child) {
                        walked.push(child);
                    }
                }
            }
        }
        at += 1;
    }
    walked
        .into_iter()
        .skip(1)
        .filter_map(Identity::read)
        .collect()
}

pub(super) fn parent_dies(
    parent: &mut Owned,
    ready: &std::path::Path,
    mode: &str,
    bubblewrap: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        parent.1 = descendants(parent.0.id());
        let sleeping = parent.1.iter().any(|child| {
            use std::os::unix::ffi::OsStrExt;
            std::fs::read(format!("/proc/{}/cmdline", child.pid)).is_ok_and(|args| {
                let program = args.split(|byte| *byte == 0).next().unwrap_or_default();
                std::path::Path::new(std::ffi::OsStr::from_bytes(program)).file_name()
                    == Some(std::ffi::OsStr::new("sleep"))
            })
        });
        if ready.exists() && sleeping {
            break;
        }
        assert!(
            parent.0.try_wait().expect("parent status").is_none(),
            "jailed child exited before readiness"
        );
        assert!(Instant::now() < deadline, "jailed child never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    parent.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    while parent.1.iter().any(Identity::alive) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        parent.1.iter().all(|child| !child.alive()),
        "{mode}, bubblewrap={bubblewrap}: jailed descendants outlived parent: {:?}",
        parent
            .1
            .iter()
            .filter(|child| child.alive())
            .map(|child| child.pid)
            .collect::<Vec<_>>()
    );
}

pub(super) fn inspect_descriptors(parent: &mut Owned, root: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("work/ready").exists() {
        assert!(parent.0.try_wait().expect("parent status").is_none());
        assert!(
            Instant::now() < deadline,
            "descriptor probe never became ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    parent.1 = descendants(parent.0.id());
    let probe = parent
        .1
        .iter()
        .find(|child| {
            std::fs::read_link(format!("/proc/{}/exe", child.pid)).is_ok_and(|path| {
                path.file_name() == Some(std::ffi::OsStr::new("descriptor-probe"))
            })
        })
        .expect("running descriptor probe");
    for entry in std::fs::read_dir(format!("/proc/{}/fd", probe.pid)).expect("descriptors") {
        let entry = entry.expect("descriptor entry");
        let target = std::fs::read_link(entry.path()).expect("descriptor target");
        assert!(
            !target.to_string_lossy().contains("casper-contained")
                && !target.to_string_lossy().contains("casper-seccomp")
                && ![
                    "/usr",
                    "/usr/bin",
                    "/usr/lib",
                    "/usr/lib64",
                    "/etc",
                    "/opt",
                    "/nix/store"
                ]
                .iter()
                .any(|path| target == std::path::Path::new(path)),
            "inherited mount descriptor: {}",
            target.display()
        );
    }
    std::fs::write(root.join("work/release"), "checked").expect("release probe");
}

#[test]
fn containment_children_leave_one_complete_parent_record_on_stdout() {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    let name = "jailed_commands_and_screens_cannot_reach_host_unix_sockets";
    let mut child = Owned::new(
        Command::new(std::env::current_exe().expect("containment executable"))
            .args(["--exact", name, "--nocapture"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("parent record probe"),
    );
    let capture = |reader: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .expect("bounded probe output");
            bytes
        })
    };
    let stdout = capture(Box::new(child.0.stdout.take().expect("probe stdout")));
    let stderr = capture(Box::new(child.0.stderr.take().expect("probe stderr")));
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("probe status") {
            break status;
        }
        assert!(Instant::now() < deadline, "parent record probe deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout.join().expect("stdout reader");
    let stderr = stderr.join().expect("stderr reader");
    assert!(stdout.len() <= 64 * 1024 && stderr.len() <= 64 * 1024);
    assert!(status.success(), "{}", String::from_utf8_lossy(&stderr));
    let stdout = String::from_utf8(stdout).expect("UTF-8 completion record");
    assert_eq!(stdout.matches("running 1 test").count(), 1, "{stdout}");
    assert_eq!(stdout.matches("test result: ok.").count(), 1, "{stdout}");
    assert!(!stdout.contains("test contained_child ..."), "{stdout}");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == format!("test {name} ... ok"))
            .count(),
        1,
        "parent completion is not independently parseable: {stdout}"
    );
}
