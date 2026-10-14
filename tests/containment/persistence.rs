use std::path::Path;
use std::time::{Duration, Instant};

const FILES: &[&str] = &[
    ".magi.lua",
    ".balthasar.lua",
    ".git/config",
    ".git/hooks/pre-commit",
];

pub(super) fn prepare(root: &Path) {
    std::fs::create_dir_all(root.join("work/.git/hooks")).expect("fixture repository");
    for file in FILES {
        std::fs::write(root.join("work").join(file), "ORIGINAL\n").expect("persistence file");
    }
}

/// Every way a shell changes a file, tried on each; the work directory itself stays writable.
fn script() -> String {
    let mut script = String::new();
    for file in FILES {
        script.push_str(&format!(
            "echo bad >> '{file}' 2>/dev/null; echo bad > '{file}' 2>/dev/null; \
             /bin/rm -f '{file}' 2>/dev/null; /bin/mv '{file}' moved 2>/dev/null; \
             echo bad > new 2>/dev/null && /bin/mv -f new '{file}' 2>/dev/null; "
        ));
    }
    script.push_str("echo CONTAINED > report");
    script
}

pub(super) fn check(root: &Path, mode: &str) {
    if mode == "screen" {
        let mut screen = casper::pty::Screen::open(
            &casper::pty::Spec {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), script()],
                ..Default::default()
            },
            24,
            80,
        )
        .expect("jailed screen");
        let deadline = Instant::now() + Duration::from_secs(10);
        while screen.read() {
            assert!(Instant::now() < deadline, "screen did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        screen.close();
    } else {
        let done = casper::running::run("/bin/sh", &["-c".into(), script()]);
        assert_eq!(done.code, 0, "{} {}", done.err, done.out);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("work/report"))
            .expect("probe ran")
            .trim(),
        "CONTAINED"
    );
    for file in FILES {
        assert_eq!(
            std::fs::read_to_string(root.join("work").join(file))
                .as_deref()
                .ok(),
            Some("ORIGINAL\n"),
            "{file} changed inside the jail"
        );
    }
}
