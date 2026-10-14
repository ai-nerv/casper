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
pub(crate) use command::worker;

/// What turns the jail on, in casper's own name as [`crate::setup`] reads its configuration. A
/// coordinator sets it on the spawn: `1` for the conservative profile, or a JSON [`Grants`] object
/// for one the session's own permissions widened. Absent, a command runs as before.
pub const WANTED: &str = "CASPER_JAIL";

/// What the session's grants add to the conservative floor, read off [`WANTED`] as JSON so casper
/// need not know how magi keeps its ledger.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// What [`WANTED`] carried: nothing when off or absent, the floor for `1`, or a widened set
    /// from JSON. Anything else is a refusal naming why.
    fn read(wanted: Option<&str>) -> Result<Option<Self>, String> {
        match wanted.map(str::trim) {
            None | Some("") => Ok(None),
            Some("1") => Ok(Some(Self::default())),
            Some(json) => serde_json::from_str(json)
                .map(Some)
                .map_err(|why| format!("{WANTED} is not a jail profile: {why}")),
        }
    }
}

/// The jail one command runs inside: whether it is on, and the paths it is built around. Read from
/// this process's environment for both the command and socket doors. Calls do not supply grants.
pub struct Jail {
    grants: Result<Option<Grants>, String>,
    cwd: PathBuf,
    home: PathBuf,
}

impl Jail {
    /// The jail this process was spawned with — the command door: profile off [`WANTED`], the cwd
    /// it inherited, the home it carries.
    #[must_use]
    pub fn from_env() -> Self {
        let wanted = std::env::var_os(WANTED).map(|v| v.to_string_lossy().into_owned());
        Self {
            grants: Grants::read(wanted.as_deref()),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
        }
    }

    /// Whether a coordinator asked for a jail at all. A profile that did not parse is one asked
    /// for, and refuses every command.
    #[must_use]
    pub const fn on(&self) -> bool {
        !matches!(self.grants, Ok(None))
    }

    pub fn allows_network(&self) -> bool {
        match &self.grants {
            Ok(grants) => grants.as_ref().is_none_or(|grants| grants.reach),
            Err(_) => false,
        }
    }
}

/// Paths beneath a writable bind that stay read-only on the bubblewrap path: what the next
/// unjailed `git`, `magi` or `balthasar` would run.
const PERSISTENT: &[&str] = &[".git/hooks", ".git/config", ".magi.lua", ".balthasar.lua"];

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
    // A writable checkout keeps its `.git`, but not what the next unjailed program would run.
    for root in std::iter::once(cwd).chain(grants.write.iter().map(PathBuf::as_path)) {
        for name in PERSISTENT {
            let path = root.join(name);
            if path.exists() {
                bind(&mut a, "--ro-bind", &path);
            }
        }
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

/// The syscalls no jailed command ever needs and that a hostile one would reach for: another
/// process's memory, `io_uring`, the kernel keyring, BPF, perf, userfaultfd, namespaces, mounts,
/// and kernel modules. Denied with `EPERM`, so a program that probes them is told no.
const FORBIDDEN: &[i64] = &[
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_userfaultfd,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
];

/// What `personality` may still be asked: the query and the ordinary Linux personas.
const PERSONAS: &[u64] = &[0, 0x8, 0x2_0000, 0x2_0008, 0xffff_ffff];

/// The syscalls an ungranted network loses.
const UNREACHED: &[i64] = &[
    libc::SYS_socket,
    libc::SYS_connect,
    libc::SYS_sendto,
    libc::SYS_sendmsg,
    libc::SYS_sendmmsg,
];

/// The jail's `EPERM` filter: [`FORBIDDEN`], `clone` into a new user namespace, an unusual
/// `personality`, the network unless `reach`, and a Unix socket unless `unix`.
fn filter(reach: bool, unix: bool) -> seccompiler::BpfProgram {
    use seccompiler::{SeccompCmpArgLen as Len, SeccompCmpOp as Op, SeccompCondition, SeccompRule};
    let on = |arg, op, value| SeccompCondition::new(arg, Len::Dword, op, value);
    let rule = |conditions: Vec<Result<SeccompCondition, _>>| {
        conditions
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .and_then(SeccompRule::new)
    };
    let newuser = libc::CLONE_NEWUSER as u64;
    let mut rules: Vec<(i64, Vec<Result<SeccompRule, _>>)> = vec![
        (
            libc::SYS_clone,
            vec![rule(vec![on(0, Op::MaskedEq(newuser), newuser)])],
        ),
        (
            libc::SYS_personality,
            vec![rule(PERSONAS.iter().map(|p| on(0, Op::Ne, *p)).collect())],
        ),
    ];
    rules.extend(FORBIDDEN.iter().map(|nr| (*nr, Vec::new())));
    if !reach {
        rules.extend(UNREACHED.iter().map(|nr| (*nr, Vec::new())));
    } else if !unix {
        let local = libc::AF_UNIX as u64;
        rules.push((libc::SYS_socket, vec![rule(vec![on(0, Op::Eq, local)])]));
    }
    let built: Result<std::collections::BTreeMap<_, _>, _> = rules
        .into_iter()
        .map(|(nr, list)| {
            list.into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map(|l| (nr, l))
        })
        .collect();
    built.map_or_else(
        |why| {
            crate::noted!("jail: a seccomp rule could not be built ({why}); none is armed");
            seccompiler::BpfProgram::default()
        },
        compile,
    )
}

/// A seccomp program that allows everything but `forbid`, each of which answers `EPERM`.
#[cfg(test)]
fn deny(forbid: &[i64]) -> seccompiler::BpfProgram {
    compile(forbid.iter().map(|nr| (*nr, Vec::new())).collect())
}

/// `rules` as a program answering `EPERM` on a match and allowing everything else.
fn compile(
    rules: std::collections::BTreeMap<i64, Vec<seccompiler::SeccompRule>>,
) -> seccompiler::BpfProgram {
    use seccompiler::{SeccompAction, SeccompFilter, TargetArch};
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

/// The jail's `ENOSYS` filter: `clone3`, whose flags seccomp cannot read, so the C library falls
/// back to `clone`; and on x86-64 every x32 call.
fn absent() -> seccompiler::BpfProgram {
    use seccompiler::sock_filter;
    const LOAD_NR: u16 = 0x20;
    const JEQ: u16 = 0x15;
    const JGE: u16 = 0x35;
    const RET: u16 = 0x06;
    const X32: u32 = 0x4000_0000;
    let at = |code, jt, jf, k| sock_filter { code, jt, jf, k };
    let enosys = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
    let mut program = vec![at(LOAD_NR, 0, 0, 0)];
    if cfg!(target_arch = "x86_64") {
        program.push(at(JGE, 2, 0, X32));
    }
    program.extend([
        at(JEQ, 1, 0, libc::SYS_clone3 as u32),
        at(RET, 0, 0, libc::SECCOMP_RET_ALLOW),
        at(RET, 0, 0, enosys),
    ]);
    program
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

/// The bubblewrap path's Landlock ruleset: no filesystem or network rule, only the abstract Unix
/// socket namespace scoped to the jail. `None` when the kernel cannot scope it.
fn scoped() -> Option<landlock::RulesetCreated> {
    use landlock::{CompatLevel, Compatible, Ruleset, RulesetAttr, Scope};
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .scope(Scope::AbstractUnixSocket)
        .ok()?
        .create()
        .ok()
}

/// A Landlock ruleset from the grants: read across the system directories, write at `cwd`, each
/// granted directory and the scratch devices, TCP denied unless a reach grant, signals scoped to
/// this domain. `/dev/tty` only for a screen, whose terminal is its own. Every requested right
/// must be supported by the running kernel.
fn ruleset(
    cwd: &Path,
    grants: &Grants,
    protected: &[PathBuf],
    terminal: bool,
) -> Option<landlock::RulesetCreated> {
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
    let tty = terminal.then_some("/dev/tty");
    for device in ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"]
        .into_iter()
        .chain(tty)
    {
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
    fn what_the_next_unjailed_program_runs_is_bound_read_only_under_every_writable_root() {
        let dir = Scratch::new("jail", "persistent");
        let (work, other) = (dir.join("work"), dir.join("other"));
        for path in [work.join(".git/hooks"), other.clone()] {
            std::fs::create_dir_all(path).expect("mkdir");
        }
        for file in [".git/config", ".magi.lua", ".balthasar.lua"] {
            std::fs::write(work.join(file), "x").expect("write");
        }
        std::fs::write(other.join(".magi.lua"), "x").expect("write");
        let grants = Grants {
            write: vec![other.clone()],
            ..Grants::default()
        };
        let a = profile(&work, &grants).join(" ");
        for path in [
            work.join(".git/hooks"),
            work.join(".git/config"),
            work.join(".magi.lua"),
            work.join(".balthasar.lua"),
            other.join(".magi.lua"),
        ] {
            let p = path.display();
            assert!(a.contains(&format!("--ro-bind {p} {p}")), "{p}: {a}");
        }
        assert!(
            !a.contains(&other.join(".git").display().to_string()),
            "{a}"
        );
        let writable = a.find(&format!("--bind {}", other.display()));
        let sealed = a.find(&format!("--ro-bind {}", other.join(".magi.lua").display()));
        assert!(writable < sealed, "the read-only bind comes after: {a}");
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
    fn a_profile_that_does_not_parse_is_a_refusal_and_not_the_floor() {
        for wanted in ["{not json", r#"{"reach":true,"wirte":["/"]}"#, "yes"] {
            let jail = Jail {
                grants: Grants::read(Some(wanted)),
                cwd: PathBuf::from("/"),
                home: PathBuf::new(),
            };
            assert!(jail.on() && !jail.allows_network(), "{wanted}");
            let refused = jail.prepare("true", &[], None, &[], false).err();
            let why = refused.map(|why| why.to_string()).unwrap_or_default();
            assert!(why.contains("not a jail profile"), "{wanted}: {why}");
        }
        assert!(matches!(Grants::read(None), Ok(None)));
        assert!(matches!(Grants::read(Some(" ")), Ok(None)));
        assert!(matches!(Grants::read(Some("1")), Ok(Some(_))));
    }

    #[test]
    fn the_real_filter_compiles_and_names_the_forbidden_calls() {
        for (reach, unix) in [(false, false), (true, false), (true, true)] {
            let program = filter(reach, unix);
            assert!(!program.is_empty(), "the seccomp program built to nothing");
        }
        assert!(!absent().is_empty());
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
        crate::tied::confine(&mut command, vec![filter]);
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
        let Some(ruleset) = ruleset(&work, &Grants::default(), &[], false) else {
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
    fn a_missing_bubblewrap_fails_the_containment_test_when_containment_is_required() {
        if std::env::var_os("CASPER_TEST_NO_BWRAP").is_some() {
            return;
        }
        let name = "jail::tests::a_jailed_command_cannot_read_the_keys_or_reach_the_network";
        let run = |required: bool| {
            let mut command = std::process::Command::new(std::env::current_exe().expect("exe"));
            command
                .args(["--exact", name, "--nocapture"])
                .env("PATH", "")
                .env("CASPER_TEST_NO_BWRAP", "1")
                .env_remove("CASPER_REQUIRE_CONTAINMENT")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if required {
                command.env("CASPER_REQUIRE_CONTAINMENT", "1");
            }
            command.status().expect("the containment test runs")
        };
        assert!(
            run(false).success(),
            "an unrequired jail is reported, not failed"
        );
        assert!(!run(true).success(), "a required jail without bwrap passed");
    }

    #[test]
    fn a_jailed_command_cannot_read_the_keys_or_reach_the_network() {
        let Some(bwrap) = which("bwrap") else {
            assert!(
                std::env::var_os("CASPER_REQUIRE_CONTAINMENT").is_none(),
                "required bubblewrap unavailable"
            );
            eprintln!("NOT VERIFIED: no bwrap to build a jail with");
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
