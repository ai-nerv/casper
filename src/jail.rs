//! A command runs inside a kernel-built jail, so a rule is a wall it cannot cross whatever it runs.
//!
//! casper is where the model's commands become processes, so it is where they are contained.
//! bubblewrap builds the world a command sees: a read-only view of the machine, writable only
//! where the work is, the credential stores masked, no network, and no sight of the user's other
//! processes. It is inherited by everything the command starts, so a script that runs a program
//! that runs a program is as bounded as the first.
//!
//! Off unless [`WANTED`] is set, so nothing changes for a session until a coordinator turns it on;
//! the grant-driven profile is layered on top later. When bubblewrap is not installed the command
//! runs unwrapped and says so — Landlock is what stands alone where bwrap cannot.

use std::path::{Path, PathBuf};

/// What turns the jail on, in casper's own name as [`crate::setup`] reads its configuration: a
/// coordinator sets it on the spawn. Absent, a command runs as before.
pub const WANTED: &str = "CASPER_JAIL";

/// The command as it should actually be started: `bwrap` and its arguments wrapping the program,
/// or the program unchanged when the jail is off or unavailable.
#[must_use]
pub fn wrap(program: &str, args: &[String]) -> (String, Vec<String>) {
    if std::env::var(WANTED).as_deref() != Ok("1") {
        return (program.to_owned(), args.to_vec());
    }
    let Some(bwrap) = which("bwrap") else {
        crate::noted!("jail: bwrap is not installed; {program} runs unsandboxed");
        return (program.to_owned(), args.to_vec());
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut argv = profile(&cwd, &home);
    // Cleared, then the few a command needs — never this process's own credentials, which is what
    // masking the stores on disk would miss.
    argv.push("--clearenv".to_owned());
    for keep in ["PATH", "HOME", "TERM", "LANG", "LC_ALL", "USER"] {
        if let Some(value) = std::env::var_os(keep).and_then(|v| v.into_string().ok()) {
            argv.extend(["--setenv".to_owned(), keep.to_owned(), value]);
        }
    }
    argv.push("--".to_owned());
    argv.push(program.to_owned());
    argv.extend(args.iter().cloned());
    (bwrap, argv)
}

/// The bubblewrap arguments for the conservative profile: the world a command sees before any grant
/// widens it. In order, because bubblewrap applies them in order and a later mount wins.
#[must_use]
pub fn profile(cwd: &Path, home: &Path) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let path = |a: &mut Vec<String>, flag: &str, p: &Path| {
        a.push(flag.to_owned());
        a.push(p.display().to_string());
    };
    let bind = |a: &mut Vec<String>, flag: &str, p: &Path| {
        a.push(flag.to_owned());
        a.push(p.display().to_string());
        a.push(p.display().to_string());
    };
    // Readable everywhere, so a build finds its toolchain; writable is layered on top.
    a.extend(["--ro-bind", "/", "/"].map(str::to_owned));
    a.extend(["--dev", "/dev"].map(str::to_owned));
    a.extend(["--proc", "/proc"].map(str::to_owned));
    path(&mut a, "--tmpfs", Path::new("/tmp"));
    bind(&mut a, "--bind", cwd);
    // The credential stores, masked though the machine is readable.
    for deny in mandatory_deny(home) {
        if deny.exists() {
            path(&mut a, "--tmpfs", &deny);
        }
    }
    // A writable checkout keeps its `.git`, but not the hooks the next `git` would run.
    let hooks = cwd.join(".git/hooks");
    if hooks.exists() {
        bind(&mut a, "--ro-bind", &hooks);
    }
    // A tool command talks to nothing over a socket, so the runtime directory is not bound in: what
    // does — a jailed child session reaching its own siblings — is the coordinator's to arrange.
    a.extend(
        [
            "--unshare-net",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
        ]
        .map(str::to_owned),
    );
    a.extend(["--die-with-parent", "--new-session"].map(str::to_owned));
    path(&mut a, "--chdir", cwd);
    a
}

/// The stores kept out of reach whatever the grants: a command that could read these could carry
/// the user's keys out however narrow the rest of its reach.
fn mandatory_deny(home: &Path) -> Vec<PathBuf> {
    [".ssh", ".aws", ".gnupg", ".docker", ".kube", ".config/gh"]
        .iter()
        .map(|p| home.join(p))
        .collect()
}

/// The first `name` on `$PATH`.
fn which(name: &str) -> Option<String> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .map(|p| p.display().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn the_profile_reads_the_world_writes_the_cwd_and_cuts_the_network() {
        let a = profile(Path::new("/w/proj"), Path::new("/home/x")).join(" ");
        assert!(a.contains("--ro-bind / /"), "{a}");
        assert!(a.contains("--bind /w/proj /w/proj"), "{a}");
        assert!(a.contains("--unshare-net"), "{a}");
        assert!(a.contains("--die-with-parent"), "{a}");
    }

    #[test]
    fn off_by_default_the_command_is_unchanged() {
        // Nothing in the suite sets it, so the default path is what runs here: the program passes
        // through untouched, which is what a session that never turned the jail on gets.
        assert_eq!(
            std::env::var(WANTED).ok().as_deref(),
            None,
            "the suite must not set {WANTED}"
        );
        let (program, args) = wrap("sh", &["-c".to_owned(), "echo hi".to_owned()]);
        assert_eq!(program, "sh");
        assert_eq!(args, ["-c", "echo hi"]);
    }

    #[test]
    fn a_jailed_command_cannot_read_the_keys_or_reach_the_network() {
        let Some(bwrap) = which("bwrap") else {
            eprintln!("skipping: no bwrap to build a jail with");
            return;
        };
        let dir = Scratch::new("jail", "escape");
        let home = dir.join("home");
        let work = dir.join("work");
        std::fs::create_dir_all(home.join(".ssh")).expect("mkdir");
        std::fs::create_dir_all(&work).expect("mkdir");
        std::fs::write(home.join(".ssh/id"), "THE-SECRET-KEY").expect("write");

        let mut argv = profile(&work, &home);
        argv.push("--clearenv".to_owned());
        argv.extend([
            "--setenv".to_owned(),
            "HOME".to_owned(),
            home.display().to_string(),
        ]);
        argv.extend([
            "--setenv".to_owned(),
            "PATH".to_owned(),
            "/usr/bin:/bin".to_owned(),
        ]);
        argv.push("--".to_owned());
        // `/proc/net/dev` is namespaced through the fresh `--proc`, unlike `/sys/class/net`, which
        // shows the host's bind-mounted sysfs; in an unshared netns it lists only the loopback.
        argv.extend(
            [
                "sh",
                "-c",
                "cat ~/.ssh/id 2>&1; echo --net--; cat /proc/net/dev",
            ]
            .map(str::to_owned),
        );

        let out = std::process::Command::new(bwrap)
            .args(&argv)
            .output()
            .expect("bwrap runs");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            !said.contains("THE-SECRET-KEY"),
            "the key was readable in the jail: {said}"
        );
        let net = said.split("--net--").nth(1).unwrap_or("");
        assert!(
            net.contains("lo:"),
            "the loopback should still be there: {said}"
        );
        assert!(
            !net.contains("eth")
                && !net.contains("wlan")
                && !net.contains("enp")
                && !net.contains("wlp"),
            "a real interface was reachable in the jail: {said}"
        );
    }
}
