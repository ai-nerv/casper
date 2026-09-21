//! A command runs inside a kernel-built jail, so a rule is a wall it cannot cross whatever it runs.
//!
//! casper is where the model's commands become processes, so it is where they are contained.
//! bubblewrap builds the world a command sees: read-only system directories, writable only
//! where the work is, no credential-store mounts, no network, and no sight of the user's other
//! processes. It is inherited by everything the command starts, so a script that runs a program
//! that runs a program is as bounded as the first.
//!
//! [`Jail::prepare`] captures the child environment and required protections for ordinary and
//! terminal children. Without bubblewrap, Landlock restricts filesystem and process access and
//! seccomp blocks ungranted sockets. Missing required protections refuse the spawn.

use std::path::{Path, PathBuf};

mod command;
mod credentials;
mod filter;
mod policy;
mod temporary;
pub use command::Prepared;

/// What turns the jail on, in casper's own name as [`crate::setup`] reads its configuration. A
/// coordinator sets it on the spawn: `1` for the conservative profile, or a JSON [`Grants`] object
/// for one the session's own permissions widened. Absent, a command runs as before.
pub const WANTED: &str = "CASPER_JAIL";

/// What the session's grants add to the conservative floor, read off [`WANTED`] as JSON so casper
/// need not know how magi keeps its ledger.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct Grants {
    /// Directories a call may write, beyond the working directory.
    #[serde(default)]
    pub write: Vec<PathBuf>,
    /// Whether unrestricted network access was granted.
    #[serde(default)]
    pub reach: bool,
    /// A host directory to mount as `/tmp`, so a project's commands share one tmp rather than each
    /// getting a fresh empty one. Absent means a private `--tmpfs`, the old behaviour. The
    /// coordinator makes the directory; casper only binds it.
    #[serde(default)]
    pub tmp: Option<PathBuf>,
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
/// this process's environment for both the command and socket doors. Calls do not supply grants.
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

    /// Whether a coordinator asked for a jail at all.
    #[must_use]
    pub const fn on(&self) -> bool {
        self.grants.is_some()
    }
}

/// The bubblewrap arguments: the conservative floor, widened by whatever the session's [`Grants`]
/// carried. In order, because bubblewrap applies them in order and a later mount wins.
#[must_use]
fn profile(cwd: &Path, grants: &Grants) -> Vec<String> {
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
    for dir in read_floor() {
        bind(&mut a, "--ro-bind", &dir);
    }
    a.extend(["--dev", "/dev"].map(str::to_owned));
    a.extend(["--proc", "/proc"].map(str::to_owned));
    // A shared `/tmp` when the coordinator gave one, so a project's commands see each other's temp
    // files; otherwise a private tmpfs, empty per command as before.
    match &grants.tmp {
        Some(shared) if shared.exists() => {
            a.push("--bind".to_owned());
            a.push(shared.display().to_string());
            a.push("/tmp".to_owned());
        }
        _ => path(&mut a, "--tmpfs", Path::new("/tmp")),
    }
    // The working directory always, and each directory a write grant named.
    bind(&mut a, "--bind", cwd);
    for writable in &grants.write {
        bind(&mut a, "--bind", writable);
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
    .unwrap_or_else(|why| {
        crate::noted!("jail: the seccomp filter could not be built ({why}); none is armed");
        seccompiler::BpfProgram::default()
    })
}

/// System directories readable in both backends, subject to credential-overlap validation.
const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/bin",
    "/sbin",
    "/etc",
    "/opt",
    "/nix/store",
];

/// Every directory a jailed command may read: the system ones, and wherever this machine keeps its
/// Rust toolchain, which is not always beneath them. A command that cannot reach the toolchain
/// cannot build anything, and where rustup puts it is the person's own arrangement.
pub(super) fn read_floor() -> Vec<PathBuf> {
    SYSTEM_READ
        .iter()
        .map(PathBuf::from)
        .chain(policy::toolchain())
        .filter(|dir| dir.exists())
        .collect()
}

/// A Landlock ruleset from the grants: read across the system directories, write at `cwd`, each
/// granted directory and the scratch devices, TCP denied unless a reach grant, signals scoped to
/// this domain. Every requested right must be supported by the running kernel.
fn ruleset(cwd: &Path, grants: &Grants, protected: &[PathBuf]) -> Option<landlock::RulesetCreated> {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr, Scope,
    };
    let abi = ABI::V6;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
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
    for dir in read_floor() {
        let fd = policy::validated_fd(&dir, protected).ok()?;
        created = created.add_rule(PathBeneath::new(fd, read)).ok()?;
    }
    let writable = std::iter::once(cwd.to_path_buf())
        .chain(grants.write.iter().cloned())
        .chain(grants.tmp.iter().cloned());
    for dir in writable {
        let fd = policy::validated_fd(&dir, protected).ok()?;
        created = created.add_rule(PathBeneath::new(fd, all)).ok()?;
    }
    for device in [
        "/dev/null",
        "/dev/zero",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
    ] {
        if let Ok(fd) = PathFd::new(device) {
            created = created
                .add_rule(PathBeneath::new(
                    fd,
                    AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::IoctlDev,
                ))
                .ok()?;
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
    fn the_floor_reads_system_directories_writes_the_cwd_and_cuts_the_network() {
        let a = profile(Path::new("/w/proj"), &Grants::default()).join(" ");
        assert!(!a.contains("--ro-bind / /"), "{a}");
        assert!(a.contains("--ro-bind /usr /usr"), "{a}");
        assert!(a.contains("--bind /w/proj /w/proj"), "{a}");
        assert!(a.contains("--unshare-net"), "{a}");
        assert!(a.contains("--die-with-parent"), "{a}");
    }

    #[test]
    fn a_grant_widens_the_floor_and_never_narrows_it() {
        let grants = Grants {
            write: vec![PathBuf::from("/w/other")],
            reach: true,
            tmp: None,
        };
        let a = profile(Path::new("/w/proj"), &grants).join(" ");
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
    fn a_shared_tmp_is_bound_over_the_private_one() {
        let dir = Scratch::new("jail", "sharedtmp");
        let grants = Grants {
            write: Vec::new(),
            reach: false,
            tmp: Some(dir.to_path_buf()),
        };
        let a = profile(Path::new("/w/proj"), &grants).join(" ");
        assert!(
            a.contains(&format!("--bind {} /tmp", dir.display())),
            "the shared tmp is bound as /tmp: {a}"
        );
        assert!(
            !a.split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|args| args == ["--tmpfs", "/tmp"]),
            "the private tmpfs is gone: {a}"
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
        let mut prepared = Jail::from_env()
            .prepare(
                "sh",
                &["-c".to_owned(), "echo hi".to_owned()],
                None,
                &[],
                false,
            )
            .expect("unjailed command");
        let command = prepared.command();
        assert_eq!(command.get_program(), "sh");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["-c", "echo hi"]);
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
        let dir = Scratch::new("jail", "landlock");
        let outside = dir.join("home");
        std::fs::create_dir_all(&outside).expect("mkdir");
        let secret = outside.join("secret");
        std::fs::write(&secret, "THE-SECRET-KEY").expect("write");

        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        let Some(ruleset) = ruleset(&work, &Grants::default(), &[]) else {
            assert!(
                std::env::var_os("CASPER_REQUIRE_CONTAINMENT").is_none(),
                "required Landlock unavailable"
            );
            eprintln!("NOT VERIFIED: Landlock unavailable");
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

        let mut argv = profile(&work, &Grants::default());
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
