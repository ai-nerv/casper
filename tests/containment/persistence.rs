use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What the next unjailed coordinator, memory or `git` runs, beneath the writable workspace.
const FILES: &[&str] = &[
    ".magi.lua",
    ".balthasar.lua",
    ".git/config",
    ".git/hooks/pre-commit",
];

/// What the person's next shell, casper or coordinator runs, outside every writable root.
pub(super) fn outside(root: &Path) -> [PathBuf; 4] {
    [
        root.join("home/.bashrc"),
        root.join("home/.gitconfig"),
        root.join("config/casper/tools.lua"),
        root.join("config").join("magi").join("init.lua"),
    ]
}

pub(super) fn prepare(root: &Path) {
    std::fs::create_dir_all(root.join("work/.git/hooks")).expect("fixture repository");
    for file in FILES {
        std::fs::write(root.join("work").join(file), "ORIGINAL\n").expect("persistence file");
    }
    for file in outside(root) {
        let parent = file.parent().expect("a parent");
        std::fs::create_dir_all(parent).expect("fixture configuration");
        std::fs::write(file, "ORIGINAL\n").expect("persistence file");
    }
}

/// Every way a shell changes a file, tried on each; the work directory itself stays writable.
fn script(root: &Path) -> String {
    let outside = outside(root).map(|file| file.display().to_string());
    let mut script = String::new();
    for file in FILES.iter().map(|file| (*file).to_owned()).chain(outside) {
        script.push_str(&format!(
            "echo bad >> '{file}' 2>/dev/null; echo bad > '{file}' 2>/dev/null; \
             /bin/rm -f '{file}' 2>/dev/null; /bin/mv '{file}' moved 2>/dev/null; \
             echo bad > new 2>/dev/null && /bin/mv -f new '{file}' 2>/dev/null; "
        ));
    }
    script.push_str("echo CONTAINED > report");
    script
}

/// Try every file from inside the jail. Outside the writable roots each stays as it was; beneath
/// them each stays only when `sealed`, which the bubblewrap path is and the Landlock path is not.
pub(super) fn check(root: &Path, mode: &str, sealed: bool) {
    if mode == "screen" {
        let mut screen = casper::pty::Screen::open(
            &casper::pty::Spec {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), script(root)],
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
        let done = casper::running::run("/bin/sh", &["-c".into(), script(root)]);
        assert_eq!(done.code, 0, "{} {}", done.err, done.out);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("work/report"))
            .expect("probe ran")
            .trim(),
        "CONTAINED"
    );
    let original =
        |path: &Path| std::fs::read_to_string(path).ok().as_deref() == Some("ORIGINAL\n");
    for file in outside(root) {
        assert!(
            original(&file),
            "{} changed inside the jail",
            file.display()
        );
    }
    for file in FILES {
        let kept = original(&root.join("work").join(file));
        assert_eq!(kept, sealed, "{file} kept={kept}, sealed={sealed}");
    }
}
