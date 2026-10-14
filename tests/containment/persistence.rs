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

fn original(path: &Path) -> bool {
    std::fs::read_to_string(path).ok().as_deref() == Some("ORIGINAL\n")
}

/// Try every file from inside the jail; each stays as it was. Beneath the writable roots the
/// bubblewrap path seals them, and the Landlock path puts them back and says so in the result.
pub(super) fn check(root: &Path, mode: &str, sealed: bool) {
    let said = if mode == "screen" {
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
        let said = screen.epitaph();
        screen.close();
        said
    } else if mode == "socket" {
        let replies = super::socket::replies(root, &script(root));
        for reply in &replies {
            assert_eq!(reply["result"][0]["failed"] == true, !sealed, "{reply}");
        }
        let said =
            |reply: &serde_json::Value| reply["result"][0]["said"].as_str().map(str::to_owned);
        replies
            .iter()
            .filter_map(said)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let done = casper::running::run("/bin/sh", &["-c".into(), script(root)]);
        let code = if sealed { 0 } else { 1 };
        assert_eq!(done.code, code, "{} {}", done.err, done.out);
        done.err
    };
    assert_eq!(
        std::fs::read_to_string(root.join("work/report"))
            .expect("probe ran")
            .trim(),
        "CONTAINED"
    );
    let [dotfile, gitconfig, tools, coordinator] = outside(root);
    for file in [dotfile, gitconfig, coordinator] {
        assert!(
            original(&file),
            "{} changed inside the jail",
            file.display()
        );
    }
    let declared = if mode == "socket" {
        super::socket::DECLARATION
    } else {
        "ORIGINAL\n"
    };
    let held = std::fs::read_to_string(&tools).unwrap_or_default();
    assert_eq!(
        held,
        declared,
        "{} changed inside the jail",
        tools.display()
    );
    for file in FILES {
        assert!(original(&root.join("work").join(file)), "{file}: {said}");
        let named = said.contains(&format!(
            "restored {file}: a jailed command may not change it"
        ));
        assert_eq!(named, !sealed, "{file}: {said}");
    }
}

/// A reader beside a Landlock-jailed command sees its change to a protected file while it runs;
/// the file is put back only once the command has ended.
pub(super) fn window(root: &Path) {
    let file = ".magi.lua";
    let work = root.join("work");
    let watched = work.clone();
    let reader = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watched.join("ready").exists() {
            assert!(Instant::now() < deadline, "the command never wrote");
            std::thread::sleep(Duration::from_millis(10));
        }
        let seen = std::fs::read_to_string(watched.join(file)).unwrap_or_default();
        std::fs::write(watched.join("release"), "").expect("release");
        seen
    });
    let script = format!(
        "echo CHANGED > {file}; : > ready; while [ ! -e release ]; do /bin/sleep 0.01; done"
    );
    let done = casper::running::run("/bin/sh", &["-c".into(), script]);
    let seen = reader.join().expect("reader");
    assert_eq!(seen, "CHANGED\n", "the reader saw the change: {}", done.err);
    assert!(original(&work.join(file)), "{}", done.err);
    let named = format!("restored {file}: a jailed command may not change it");
    assert!(done.err.contains(&named), "{}", done.err);
}
