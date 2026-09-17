use std::path::{Path, PathBuf};

pub(super) fn unsafe_program(root: &Path, real: &Path, hardlink: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let fake = root.join("work/launcher");
    if hardlink {
        let original = root.join("bin/original-launcher");
        std::fs::copy(real, &original).expect("owned launcher copy");
        std::fs::hard_link(&original, fake).expect("writable launcher alias");
        original
    } else {
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n/bin/cat '{}/home/.external' > '{}/work/report'\n",
                root.display(),
                root.display()
            ),
        )
        .expect("synthetic launcher");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("executable launcher");
        fake
    }
}

pub(super) fn check(root: &Path, terminal: bool, retarget: bool) {
    let result = casper::jail::Jail::from_env().prepare(
        "/bin/sh",
        &["-c".into(), "echo safe > report".into()],
        None,
        &[],
        terminal,
    );
    if !retarget {
        assert!(
            result
                .err()
                .expect("unsafe launcher must refuse preparation")
                .to_string()
                .contains("launcher")
        );
        assert!(!root.join("work/report").exists());
        return;
    }
    let mut prepared = result.expect("safe captured launcher");
    let fake = unsafe_program(root, Path::new("/bin/false"), false);
    std::fs::remove_file(root.join("bin/bwrap")).expect("original PATH alias");
    std::os::unix::fs::symlink(fake, root.join("bin/bwrap")).expect("retargeted PATH alias");
    let (pty, pts) = pty_process::blocking::open().expect("terminal");
    let child = if terminal {
        prepared.screen().spawn(pts).expect("terminal child")
    } else {
        prepared.command().spawn().expect("ordinary child")
    };
    let mut child = super::lifecycle::Owned::new(child);
    assert!(child.0.wait().expect("child exit").success());
    assert_eq!(
        std::fs::read_to_string(root.join("work/report")).expect("report"),
        "safe\n",
        "the PATH alias substituted the launcher"
    );
    drop(pty);
}
