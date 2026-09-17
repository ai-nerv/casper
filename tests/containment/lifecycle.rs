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
            if identity.alive()
                && let Some(pid) = rustix::process::Pid::from_raw(identity.pid as i32)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
        self.stop();
    }
}

struct Identity {
    pid: u32,
    born: String,
}

impl Identity {
    fn read(pid: u32) -> Option<Self> {
        let (_, born) = state(pid)?;
        Some(Self { pid, born })
    }

    fn alive(&self) -> bool {
        state(self.pid).is_some_and(|(state, born)| state != "Z" && born == self.born)
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

pub(super) fn parent_dies(parent: &mut Owned, ready: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        parent.1 = descendants(parent.0.id());
        let sleeping = parent.1.iter().any(|child| {
            std::fs::read_to_string(format!("/proc/{}/comm", child.pid))
                .is_ok_and(|name| name.trim() == "sleep")
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
        "jailed descendant outlived its parent"
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
