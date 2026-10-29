#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const PROBE: &str = "jail::command::workflow::tests::probe";

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn probe() {
    let Some(root) = std::env::var_os("WORKFLOW_PROBE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(
        std::fs::read(root.join("review/SKILL.md")).unwrap(),
        b"workflow"
    );
    assert!(std::fs::write(root.join("created"), b"forbidden").is_err());
    assert!(std::fs::write(root.join("review/SKILL.md"), b"overwritten").is_err());
    assert!(std::fs::remove_file(root.join("review/SKILL.md")).is_err());
    assert!(std::fs::rename(root.join("review/SKILL.md"), root.join("moved")).is_err());
    assert!(std::fs::create_dir(root.join("new-dir")).is_err());
    let config = root.parent().unwrap();
    assert!(std::fs::read(config.join("init.lua")).is_err());
    assert!(std::fs::write(config.join("init.lua"), b"forbidden").is_err());
    assert!(std::fs::read(std::env::var_os("WORKFLOW_PROBE_SECRET").unwrap()).is_err());
    assert!(std::net::TcpListener::bind("127.0.0.1:0").is_err());
    assert!(std::os::unix::net::UnixDatagram::unbound().is_err());
    assert!(std::env::var_os("WORKFLOW_PARENT_SECRET").is_none());
    assert!(std::env::var_os("CASPER_JAIL").is_none());
    assert!(std::env::var_os("PATH").is_none());
}

fn exercise(bwrap: bool) {
    let fixture = crate::scratch::Scratch::new("workflow", "read-only-jail");
    let root = fixture.join("config").join("magi").join("skills");
    std::fs::create_dir_all(root.join("review")).unwrap();
    std::fs::write(root.join("review/SKILL.md"), b"workflow").unwrap();
    std::fs::write(root.parent().unwrap().join("init.lua"), b"configuration").unwrap();
    std::fs::write(fixture.join("secret"), b"FIXTURE_SECRET").unwrap();
    let executable = std::env::current_exe().unwrap();
    let launcher = if bwrap {
        Some(PathBuf::from(
            super::super::super::which("bwrap").expect("bwrap prerequisite"),
        ))
    } else {
        None
    };
    let mut prepared = prepare(
        &root,
        &executable,
        &["--exact".into(), PROBE.into(), "--nocapture".into()],
        launcher.as_deref(),
    )
    .unwrap();
    prepared.inner(&[
        ("WORKFLOW_PROBE_ROOT".into(), root.to_str().unwrap().into()),
        (
            "WORKFLOW_PROBE_SECRET".into(),
            fixture.join("secret").to_str().unwrap().into(),
        ),
    ]);
    let mut child = Owned(
        prepared
            .command()
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "workflow jail probe timeout");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(status.success(), "workflow probe {status}");
    assert_eq!(
        std::fs::read(root.join("review/SKILL.md")).unwrap(),
        b"workflow"
    );
    assert_eq!(
        std::fs::read(root.parent().unwrap().join("init.lua")).unwrap(),
        b"configuration"
    );
    assert!(prepared.restored().is_empty());
}

#[test]
fn landlock_can_only_read_the_selected_skill_subtree() {
    exercise(false);
}

#[test]
fn bubblewrap_can_only_read_the_selected_skill_subtree() {
    exercise(true);
}

#[test]
fn unsafe_or_relative_root_is_refused_before_spawn() {
    let fixture = crate::scratch::Scratch::new("workflow", "jail-root");
    let root = fixture.join("root");
    std::fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(&root, fixture.join("alias")).unwrap();
    let executable = std::env::current_exe().unwrap();
    for root in [
        PathBuf::from("relative"),
        fixture.join("alias"),
        fixture.join("root/.."),
        PathBuf::from(format!("/{}", "a".repeat(4096))),
    ] {
        assert!(prepare(&root, &executable, &[], None).is_err());
    }
}
