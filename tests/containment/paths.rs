use casper::scratch::Scratch;
use std::path::{Path, PathBuf};

pub(super) struct Fixture {
    root: PathBuf,
    _name: Scratch,
}

impl Fixture {
    pub(super) fn new(mode: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let name = Scratch::new("casper-contained", mode);
        let root = Path::new("/var/tmp").join(name.file_name().expect("fixture name"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("fixture outside private /tmp");
        Self { root, _name: name }
    }
}

impl std::ops::Deref for Fixture {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(super) fn create_after_ready(child: &mut super::lifecycle::Owned, root: &Path) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("work/ready").exists() {
        assert!(child.0.try_wait().expect("child status").is_none());
        assert!(Instant::now() < deadline, "child never reached barrier");
        std::thread::sleep(Duration::from_millis(10));
    }
    for path in [
        "home/.ssh/id",
        "data/magi/credentials.json",
        "home/.external",
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("late store");
        std::fs::write(path, "SYNTHETIC_SECRET\n").expect("late secret");
    }
    std::fs::write(root.join("work/release"), "ready").expect("release child");
}

pub(super) fn replaced_grant(root: &Path, terminal: bool, directory: bool) {
    let script = format!(
        "contents=$(/bin/cat '{}/grant/credentials.json' 2>/dev/null || true); case \"$contents\" in *SYNTHETIC_SECRET*) exit 11;; esac; echo safe > report",
        root.display()
    );
    let mut prepared = casper::jail::Jail::from_env()
        .prepare("/bin/sh", &["-c".into(), script], None, &[], terminal)
        .expect("captured grant");
    if directory {
        std::fs::rename(root.join("grant"), root.join("old-grant")).expect("original directory");
    } else {
        std::fs::remove_file(root.join("grant")).expect("original alias");
    }
    std::os::unix::fs::symlink(root.join("data/magi"), root.join("grant")).expect("retarget grant");
    let (pty, pts) = pty_process::blocking::open().expect("test terminal");
    let child = if terminal {
        prepared.screen().spawn(pts).expect("terminal child")
    } else {
        prepared.command().spawn().expect("ordinary child")
    };
    let mut child = super::lifecycle::Owned::new(child);
    assert!(
        child.0.wait().expect("exit").success(),
        "replaced grant leaked"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("work/report")).expect("ran"),
        "safe\n"
    );
    drop(pty);
}
