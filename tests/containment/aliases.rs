use std::path::Path;

pub(super) fn refused(variant: &str) -> bool {
    matches!(
        variant,
        "hardlinked-credential" | "hardlinked-nested" | "nested-credential" | "nested-directory"
    )
}

pub(super) fn prepare(root: &Path, variant: &str) {
    let exposed = root.join("work/exposed");
    match variant {
        "hardlinked-credential" | "hardlinked-nested" => {
            let original = if variant == "hardlinked-credential" {
                "home/.external"
            } else {
                "data/magi/credentials.json"
            };
            std::fs::hard_link(root.join(original), exposed)
                .expect("synthetic credential hardlink");
        }
        "nested-credential" => {
            std::fs::write(&exposed, "SYNTHETIC_SECRET\n").expect("synthetic target");
            std::fs::remove_file(root.join("home/.ssh/id")).expect("original key");
            std::os::unix::fs::symlink(exposed, root.join("home/.ssh/id"))
                .expect("nested key alias");
        }
        "nested-directory" => {
            std::fs::write(&exposed, "SYNTHETIC_SECRET\n").expect("synthetic target");
            std::os::unix::fs::symlink(root.join("work"), root.join("home/.ssh/keys"))
                .expect("nested store alias");
        }
        "safe-nested-alias" => {
            std::fs::create_dir(root.join("separate")).expect("external credential directory");
            let target = root.join("separate/secret");
            std::fs::write(&target, "SYNTHETIC_SECRET\n").expect("synthetic external secret");
            std::fs::remove_file(root.join("home/.ssh/id")).expect("original key");
            std::os::unix::fs::symlink(&target, root.join("home/.ssh/id"))
                .expect("external key alias");
            std::os::unix::fs::symlink(target, exposed).expect("workspace key alias");
            std::os::unix::fs::symlink(root.join("home/.ssh"), root.join("home/.ssh/back"))
                .expect("credential directory cycle");
        }
        _ => {}
    }
}

pub(super) fn check_refusal(root: &Path, mode: &str) {
    let script = "/bin/cat exposed > report";
    match mode {
        "socket" => super::socket::run(root, script, true),
        "screen" => {
            let result = casper::pty::Screen::open(
                &casper::pty::Spec {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    ..Default::default()
                },
                24,
                80,
            );
            match result {
                Err(error) => assert!(error.to_string().contains("credential"), "{error}"),
                Ok(mut screen) => {
                    screen.close();
                    panic!("credential alias must refuse terminal preparation");
                }
            }
        }
        _ => {
            let done = casper::lua::exec::run("/bin/sh", &["-c".into(), script.into()]);
            assert_ne!(done.code, 0, "credential alias was exposed");
            assert!(done.err.contains("credential"), "{}", done.err);
        }
    }
    assert!(
        !root.join("work/report").exists(),
        "unsafe command was started"
    );
}
