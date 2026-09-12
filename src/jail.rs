//! A command runs inside a kernel-built jail, so a rule is a wall it cannot cross whatever it runs.
//!
//! casper is where the model's commands become processes, so it is where they are contained.
//! bubblewrap builds the world a command sees: a read-only view of the machine, writable only
//! where the work is, the credential stores masked, no network, and no sight of the user's other
//! processes. It is inherited by everything the command starts, so a script that runs a program
//! that runs a program is as bounded as the first.
//!
//! Beside bubblewrap's world, a seccomp filter ([`Jail::seccomp`]) denies the syscalls no command
//! needs and a hostile one wants — reading another process's memory, `io_uring` — and it holds even
//! where there is no bubblewrap. Where there is none, [`Jail::landlock`] adds Landlock's filesystem,
//! network and signal walls in-process, the one containment that stands without a namespace. Both
//! are built here as data and armed in [`crate::tied`], the crate's one fork/exec window. Off unless
//! [`WANTED`] is set, so nothing changes for a session until a coordinator turns it on.

use std::path::{Path, PathBuf};

/// What turns the jail on, in casper's own name as [`crate::setup`] reads its configuration. A
/// coordinator sets it on the spawn: `1` for the conservative profile, or a JSON [`Grants`] object
/// for one the session's own permissions widened. Absent, a command runs as before.
pub const WANTED: &str = "CASPER_JAIL";

/// What the session's grants add to the conservative floor, read off [`WANTED`] as JSON so casper
/// need not know how magi keeps its ledger.
#[derive(Debug, Default, serde::Deserialize)]
pub struct Grants {
    /// Directories a call may write, beyond the working directory.
    #[serde(default)]
    pub write: Vec<PathBuf>,
    /// Whether any network was granted. bubblewrap's is all-or-nothing; per-host is Landlock's job.
    #[serde(default)]
    pub reach: bool,
}

impl Grants {
    /// What [`WANTED`] carried: nothing when off, the floor for `1`, or a widened set from JSON.
    /// Anything unparseable is the floor, never wider.
    fn read() -> Option<Self> {
        match std::env::var(WANTED).ok()?.trim() {
            "" => None,
            "1" => Some(Self::default()),
            json => Some(serde_json::from_str(json).unwrap_or_else(|why| {
                crate::noted!("jail: {WANTED} is not a profile ({why}); using the floor");
                Self::default()
            })),
        }
    }
}

/// The jail one command runs inside: whether it is on, and the paths it is built around. Read from
/// this process's environment for the command door ([`Jail::from_env`]); a socket door reads the
/// same from the connecting peer instead, so the walls are the coordinator's either way and never
/// the call's.
pub struct Jail {
    grants: Option<Grants>,
    cwd: PathBuf,
    home: PathBuf,
}

impl Jail {
    /// The jail this process was spawned with — the command door: profile off [`WANTED`], the cwd
    /// it inherited, the home it carries.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            grants: Grants::read(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
        }
    }

    /// The command as it should actually be started: `bwrap` and its arguments wrapping the
    /// program, or the program unchanged when the jail is off or unavailable.
    #[must_use]
    pub fn wrap(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        let Some(grants) = self.grants.as_ref() else {
            return (program.to_owned(), args.to_vec());
        };
        let Some(bwrap) = which("bwrap") else {
            crate::noted!("jail: bwrap is not installed; {program} runs unsandboxed");
            return (program.to_owned(), args.to_vec());
        };
        let mut argv = profile(&self.cwd, &self.home, grants);
        // Cleared, then the few a command needs — never this process's own credentials, which is
        // what masking the stores on disk would miss.
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
}

/// The bubblewrap arguments: the conservative floor, widened by whatever the session's [`Grants`]
/// carried. In order, because bubblewrap applies them in order and a later mount wins.
#[must_use]
pub fn profile(cwd: &Path, home: &Path, grants: &Grants) -> Vec<String> {
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
    // The working directory always, and each directory a write grant named.
    bind(&mut a, "--bind", cwd);
    for writable in &grants.write {
        bind(&mut a, "--bind", writable);
    }
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
    // No network unless a reach grant asked for it; a tool command talks to no socket otherwise,
    // so the runtime directory is not bound in — a jailed child session's siblings are magi's to
    // arrange.
    if !grants.reach {
        a.push("--unshare-net".to_owned());
    }
    a.extend(["--unshare-pid", "--unshare-ipc", "--unshare-uts"].map(str::to_owned));
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

impl Jail {
    /// The seccomp filter for a jailed command, or `None` with the jail off. Built here; armed in
    /// [`crate::tied::confine`] as a `pre_exec` step, inherited across `exec`, so it holds for the
    /// command bubblewrap goes on to run — and even where there is no bubblewrap, the one wall that
    /// stands without namespaces.
    #[must_use]
    pub fn seccomp(&self) -> Option<seccompiler::BpfProgram> {
        self.grants.as_ref()?;
        Some(deny(FORBIDDEN))
    }
}

/// The syscalls no jailed command ever needs and that a hostile one would reach for: reading
/// another process's memory, and `io_uring`, which can open files and sockets without the syscalls
/// the rest of the jail watches. Denied with `EPERM`, so a program that probes them is told no.
const FORBIDDEN: &[i64] = &[
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

/// A seccomp program that allows everything but `forbid`, each of which answers `EPERM`.
fn deny(forbid: &[i64]) -> seccompiler::BpfProgram {
    use seccompiler::{SeccompAction, SeccompFilter, TargetArch};
    let rules = forbid.iter().map(|nr| (*nr, Vec::new())).collect();
    let arch = if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        TargetArch::x86_64
    };
    SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .and_then(std::convert::TryInto::try_into)
    .unwrap_or_default()
}

impl Jail {
    /// The Landlock ruleset for a jailed command when bubblewrap will not build the world — the one
    /// path where the filesystem, network and signal walls must stand without a mount namespace.
    /// `None` when bwrap is present (it contains the command instead), when the jail is off, or on a
    /// kernel without Landlock. Built here; armed in [`crate::tied::restrict`].
    #[must_use]
    pub fn landlock(&self) -> Option<landlock::RulesetCreated> {
        let grants = self.grants.as_ref()?;
        if which("bwrap").is_some() {
            return None;
        }
        ruleset(&self.cwd, grants)
    }
}

/// The system directories a command reads to run at all — never `$HOME`, so the credential stores
/// under it stay unreadable the way bubblewrap's mask makes them.
const SYSTEM_READ: &[&str] = &[
    "/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/opt", "/proc", "/sys", "/run",
];

/// A Landlock ruleset from the grants: read across the system directories, write at `cwd`, each
/// granted directory and the scratch devices, TCP denied unless a reach grant, signals scoped to
/// this domain. Best-effort, so an older kernel keeps the walls it can rather than failing.
fn ruleset(cwd: &Path, grants: &Grants) -> Option<landlock::RulesetCreated> {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr, Scope,
    };
    let abi = ABI::V5;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(abi))
        .ok()?
        .scope(Scope::Signal | Scope::AbstractUnixSocket)
        .ok()?;
    if !grants.reach {
        ruleset = ruleset
            .handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp)
            .ok()?;
    }
    let mut created = ruleset.create().ok()?;
    let (read, all) = (AccessFs::from_read(abi), AccessFs::from_all(abi));
    for dir in SYSTEM_READ {
        if let Ok(fd) = PathFd::new(dir) {
            created = created.add_rule(PathBeneath::new(fd, read)).ok()?;
        }
    }
    let writable = std::iter::once(cwd.to_path_buf())
        .chain(grants.write.iter().cloned())
        .chain([PathBuf::from("/tmp"), PathBuf::from("/dev")]);
    for dir in writable {
        if let Ok(fd) = PathFd::new(&dir) {
            created = created.add_rule(PathBeneath::new(fd, all)).ok()?;
        }
    }
    Some(created)
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
    fn the_floor_reads_the_world_writes_the_cwd_and_cuts_the_network() {
        let a = profile(
            Path::new("/w/proj"),
            Path::new("/home/x"),
            &Grants::default(),
        )
        .join(" ");
        assert!(a.contains("--ro-bind / /"), "{a}");
        assert!(a.contains("--bind /w/proj /w/proj"), "{a}");
        assert!(a.contains("--unshare-net"), "{a}");
        assert!(a.contains("--die-with-parent"), "{a}");
    }

    #[test]
    fn a_grant_widens_the_floor_and_never_narrows_it() {
        let grants = Grants {
            write: vec![PathBuf::from("/w/other")],
            reach: true,
        };
        let a = profile(Path::new("/w/proj"), Path::new("/home/x"), &grants).join(" ");
        assert!(
            a.contains("--bind /w/proj /w/proj"),
            "the cwd is still writable: {a}"
        );
        assert!(
            a.contains("--bind /w/other /w/other"),
            "the granted dir is writable: {a}"
        );
        assert!(
            !a.contains("--unshare-net"),
            "a reach grant leaves the network on: {a}"
        );
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
        let (program, args) = Jail::from_env().wrap("sh", &["-c".to_owned(), "echo hi".to_owned()]);
        assert_eq!(program, "sh");
        assert_eq!(args, ["-c", "echo hi"]);
    }

    #[test]
    fn the_real_filter_compiles_and_names_the_forbidden_calls() {
        let program = deny(FORBIDDEN);
        assert!(!program.is_empty(), "the seccomp program built to nothing");
    }

    #[test]
    fn a_denied_syscall_is_refused_by_the_filter() {
        // The mechanism, proven on a syscall a shell reaches easily: a filter denying `mkdir`
        // makes `mkdir` fail with the errno the filter names, armed the way `tied::confine` arms it.
        let filter = deny(&[libc::SYS_mkdir, libc::SYS_mkdirat]);
        let dir = crate::scratch::Scratch::new("jail", "seccomp");
        let target = dir.join("nope");
        let mut command = std::process::Command::new("mkdir");
        command.arg(&target);
        crate::tied::confine(&mut command, filter);
        let status = command.status().expect("mkdir runs");
        assert!(!status.success(), "mkdir was not blocked by the filter");
        assert!(
            !target.exists(),
            "the directory was created despite the filter"
        );
    }

    #[test]
    fn landlock_alone_denies_a_read_outside_the_set_and_keeps_the_cwd_writable() {
        // The degraded path: no bwrap, so Landlock is the only wall, built and armed the way
        // `tied::restrict` arms it. The ruleset grants /tmp, so the secret goes under $HOME — the
        // credential tree Landlock exists to close — proved unreadable while the cwd stays writable.
        let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| !h.as_os_str().is_empty())
        else {
            eprintln!("skipping: no HOME to hide a secret under");
            return;
        };
        struct Hidden(PathBuf);
        impl Drop for Hidden {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let outside = home.join(format!(".casper-lltest-{}", std::process::id()));
        std::fs::create_dir_all(&outside).expect("mkdir");
        let _hidden = Hidden(outside.clone());
        let secret = outside.join("secret");
        std::fs::write(&secret, "THE-SECRET-KEY").expect("write");

        let dir = Scratch::new("jail", "landlock");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        let Some(ruleset) = ruleset(&work, &Grants::default()) else {
            eprintln!("skipping: no Landlock here");
            return;
        };
        let script = format!(
            "cat {} 2>&1; echo --sep--; echo ok > {}/w 2>&1 && echo WROTE || echo NOWRITE",
            secret.display(),
            work.display()
        );
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(&script);
        crate::tied::restrict(&mut command, ruleset);
        let out = command.output().expect("sh runs");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            !said.contains("THE-SECRET-KEY"),
            "the secret was readable under Landlock: {said}"
        );
        assert!(said.contains("WROTE"), "the cwd was not writable: {said}");
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

        let mut argv = profile(&work, &home, &Grants::default());
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
