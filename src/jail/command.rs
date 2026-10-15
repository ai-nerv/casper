//! A captured jail policy shared by ordinary and terminal children.

use super::temporary::Temporary;
use super::{Jail, policy};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Launch the current Casper executable with inherited configuration and grants.
pub(crate) fn worker(root: &Path) -> std::io::Result<tokio::process::Command> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.current_dir(root);
    crate::tied::running(&mut command, Vec::new());
    Ok(command.into())
}

/// Set when a jail that cannot be built in full must refuse rather than run with less.
const REQUIRED: &str = "CASPER_REQUIRE_CONTAINMENT";

pub struct Prepared {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    clear: bool,
    filters: Vec<seccompiler::BpfProgram>,
    landlock: Option<landlock::RulesetCreated>,
    descriptors: Vec<std::os::fd::OwnedFd>,
    guard: Option<super::guard::Guard>,
    /// Where a launcher's own arguments end: the `--` before the program, under bubblewrap.
    separator: Option<usize>,
    _temporary: Option<Temporary>,
}

impl Jail {
    /// Prepare an executable, child environment, and required kernel protections.
    pub fn prepare(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        extra: &[(String, String)],
        terminal: bool,
    ) -> std::io::Result<Prepared> {
        let mut prepared = Prepared {
            program: program.into(),
            args: args.into(),
            cwd: cwd.unwrap_or(&self.cwd).into(),
            env: extra.iter().map(|(k, v)| (k.into(), v.into())).collect(),
            clear: self.on(),
            filters: Vec::new(),
            landlock: None,
            descriptors: Vec::new(),
            guard: None,
            separator: None,
            _temporary: None,
        };
        let refused = |why: &String| std::io::Error::other(why.clone());
        let Some(grants) = self.grants.as_ref().map_err(refused)? else {
            return Ok(prepared);
        };
        let protected = policy::protected(&self.home)?;
        policy::validate(&self.cwd, grants, &protected)?;
        let bwrap = super::which("bwrap");
        let tmp = if bwrap.is_some() {
            PathBuf::from("/tmp")
        } else if let Some(tmp) = &grants.tmp {
            std::fs::canonicalize(tmp)?
        } else {
            let temporary = Temporary::new()?;
            let path = temporary.0.clone();
            prepared._temporary = Some(temporary);
            path
        };
        prepared.env = policy::environment(&self.home, &tmp, extra);
        let grammars = grammars(program, args, &crate::shelf::Shelf::from_env());
        if let Some(roots) = std::env::join_paths(grammars.iter().filter_map(|bin| bin.parent()))
            .ok()
            .filter(|roots| !roots.is_empty())
        {
            prepared.env.push((crate::shelf::NAMED.into(), roots));
        }
        let filters = vec![
            super::filter(grants.reach),
            super::filter::families(bwrap.is_some()),
            super::absent(),
        ];
        if filters.iter().any(Vec::is_empty) {
            return Err(std::io::Error::other("required seccomp filter unavailable"));
        }
        if let Some(bwrap) = bwrap {
            use std::os::fd::AsRawFd;
            let bwrap = policy::launcher(Path::new(&bwrap), &protected)?;
            let mut wrapped = super::profile(&self.cwd, grants);
            for run in itself(program).iter().chain(&grammars) {
                let (at, run) = (wrapped.len() - 2, run.display().to_string());
                wrapped.splice(at..at, ["--ro-bind".to_owned(), run.clone(), run]);
            }
            prepared.descriptors = policy::bindings(&mut wrapped, &protected, &bwrap)?;
            wrapped.truncate(wrapped.len() - 2);
            if terminal {
                wrapped.retain(|arg| arg != "--new-session");
            }
            wrapped.extend(["--chdir".into(), prepared.cwd.display().to_string()]);
            for filter in &filters {
                let descriptor = super::filter::descriptor(filter)?;
                wrapped.extend([
                    "--add-seccomp-fd".into(),
                    descriptor.as_raw_fd().to_string(),
                ]);
                prepared.descriptors.push(descriptor);
            }
            prepared.separator = Some(wrapped.len());
            wrapped.push("--".into());
            wrapped.push(program.into());
            wrapped.extend(args.iter().cloned());
            prepared.landlock = scope(&bwrap)?;
            prepared.program = bwrap;
            prepared.args = wrapped;
        } else {
            prepared.filters = filters;
            let mut granted = grants.clone();
            granted.tmp = Some(tmp);
            policy::validate(&self.cwd, &granted, &protected)?;
            let mut ruleset = super::ruleset(&self.cwd, &granted, &protected, terminal)
                .ok_or_else(|| std::io::Error::other("required Landlock protection unavailable"))?;
            for run in itself(program).iter().chain(&grammars) {
                ruleset = policy::executable(ruleset, run, &protected)?;
            }
            prepared.landlock = Some(ruleset);
            let persistent = super::persistent(&self.cwd, grants);
            prepared.guard = Some(super::guard::Guard::take(&self.cwd, persistent)?);
        }
        Ok(prepared)
    }
}

/// The program, when it is this executable: made readable inside the jail, so casper can run itself
/// there to parse what it must not parse outside.
fn itself(program: &str) -> Option<PathBuf> {
    let own = std::env::current_exe().ok()?;
    (Path::new(program) == own).then_some(own)
}

/// The grammar directories on `shelf` that `program` may read and run from: every one for
/// `casper code`, and only its own when it is a grammar's program.
fn grammars(program: &str, args: &[String], shelf: &crate::shelf::Shelf) -> Vec<PathBuf> {
    let bins = shelf.bins();
    if itself(program).is_some() && args.first().is_some_and(|verb| verb == "code") {
        return bins;
    }
    let Ok(program) = std::fs::canonicalize(program) else {
        return Vec::new();
    };
    bins.into_iter()
        .filter(|bin| program.parent() == Some(bin.as_path()))
        .collect()
}

/// The abstract-socket scope bubblewrap runs under, so the jail cannot reach the host's abstract
/// namespace. Without kernel support it is skipped, or refused when [`REQUIRED`] is set; so is a
/// setuid launcher, which `no_new_privs` would stop.
fn scope(bwrap: &Path) -> std::io::Result<Option<landlock::RulesetCreated>> {
    use std::os::unix::fs::PermissionsExt;
    let setuid = std::fs::metadata(bwrap)?.permissions().mode() & 0o4000 != 0;
    let scoped = if setuid { None } else { super::scoped() };
    if scoped.is_none() {
        if std::env::var_os(REQUIRED).is_some() {
            return Err(std::io::Error::other(
                "required abstract Unix socket scoping unavailable",
            ));
        }
        crate::noted!("jail: abstract Unix sockets are not scoped (setuid launcher={setuid})");
    }
    Ok(scoped)
}

impl Prepared {
    /// On the Landlock path, put back what the command changed beneath its writable roots that
    /// bubblewrap would have bound read-only, one line for each. Call once it has ended.
    #[must_use]
    pub fn restored(&self) -> Vec<String> {
        self.guard
            .as_ref()
            .map(super::guard::Guard::restore)
            .unwrap_or_default()
    }

    pub fn asynchronous(&mut self) -> tokio::process::Command {
        self.command().into()
    }

    /// Hand `env` to the program and never to a launcher: under bubblewrap each pair becomes a
    /// `--setenv` ahead of the program, so bubblewrap itself runs on the sanitised environment.
    /// A loader variable (`LD_*`) and a name no environment can hold are dropped on every path.
    pub fn inner(&mut self, env: &[(String, String)]) {
        let carried = env.iter().filter(|(key, _)| {
            !key.is_empty() && !key.starts_with("LD_") && !key.contains(['=', '\0'])
        });
        let Some(at) = self.separator else {
            self.env
                .extend(carried.map(|(key, value)| (key.into(), value.into())));
            return;
        };
        let tail = self.args.split_off(at.min(self.args.len()));
        for (key, value) in carried {
            self.args
                .extend(["--setenv".to_owned(), key.clone(), value.clone()]);
        }
        self.separator = Some(self.args.len());
        self.args.extend(tail);
    }

    /// Configure an ordinary child with the captured policy.
    pub fn command(&mut self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args).current_dir(&self.cwd);
        if self.clear {
            command.env_clear();
        }
        command.envs(self.env.iter().cloned());
        let filters = std::mem::take(&mut self.filters);
        if !filters.is_empty() {
            crate::tied::confine(&mut command, filters);
        }
        if let Some(ruleset) = self.landlock.take() {
            crate::tied::restrict(&mut command, ruleset);
        }
        crate::tied::running(&mut command, std::mem::take(&mut self.descriptors));
        command
    }

    /// Configure a terminal child with the same policy and its own session leadership.
    pub fn screen(&mut self) -> pty_process::blocking::Command {
        let mut command = pty_process::blocking::Command::new(&self.program)
            .args(&self.args)
            .current_dir(&self.cwd);
        if self.clear {
            command = command.env_clear();
        }
        command = command.envs(self.env.iter().cloned());
        crate::tied::on_a_screen(
            command,
            std::mem::take(&mut self.filters),
            self.landlock.take(),
            std::mem::take(&mut self.descriptors),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// A program as `prepare` leaves it, with or without bubblewrap in front.
    fn prepared(launcher: bool) -> Prepared {
        let (program, args, separator) = if launcher {
            (
                "bwrap",
                vec!["--unshare-pid", "--", "server", "--stdio"],
                Some(1),
            )
        } else {
            ("server", vec!["--stdio"], None)
        };
        Prepared {
            program: program.into(),
            args: args.into_iter().map(str::to_owned).collect(),
            cwd: PathBuf::from("/"),
            env: vec![("PATH".into(), "/usr/bin".into())],
            clear: true,
            filters: Vec::new(),
            landlock: None,
            descriptors: Vec::new(),
            guard: None,
            separator,
            _temporary: None,
        }
    }

    /// What an MCP server definition might name: one variable, two loader variables, one name no
    /// environment holds, and a `PATH` of its own.
    fn server() -> Vec<(String, String)> {
        [
            ("TOKEN", "fixture"),
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("LD_LIBRARY_PATH", "/tmp"),
            ("A=B", "c"),
            ("PATH", "/opt/bin"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .into()
    }

    #[test]
    fn a_servers_environment_reaches_the_program_and_never_its_launcher() {
        let mut prepared = prepared(true);
        prepared.inner(&server());
        let command = prepared.command();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--unshare-pid",
                "--setenv",
                "TOKEN",
                "fixture",
                "--setenv",
                "PATH",
                "/opt/bin",
                "--",
                "server",
                "--stdio"
            ]
        );
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            [(OsStr::new("PATH"), Some(OsStr::new("/usr/bin")))],
            "bubblewrap runs on the sanitised environment alone"
        );
    }

    #[test]
    fn without_a_launcher_the_program_takes_it_less_any_loader_variable() {
        let mut prepared = prepared(false);
        prepared.inner(&server());
        let command = prepared.command();
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--stdio"]);
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env.get(OsStr::new("TOKEN")),
            Some(&Some(OsStr::new("fixture")))
        );
        assert_eq!(
            env.get(OsStr::new("PATH")),
            Some(&Some(OsStr::new("/opt/bin")))
        );
        assert_eq!(env.len(), 2, "{env:?}");
    }

    #[test]
    fn only_code_and_a_grammars_own_program_reach_the_grammar_directories() {
        use crate::scratch::Scratch;
        let (a, b) = (Scratch::new("grants", "a"), Scratch::new("grants", "b"));
        for root in [&a, &b] {
            std::fs::create_dir(root.join("bin")).expect("bin");
        }
        std::fs::write(b.join("bin/rust"), "").expect("rust");
        let gone = PathBuf::from("/nonexistent");
        let shelf = crate::shelf::Shelf::at(vec![a.to_path_buf(), b.to_path_buf(), gone]);
        let bins = |program: &str, args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            grammars(program, &args, &shelf)
        };
        let bin = |root: &Scratch| std::fs::canonicalize(root.join("bin")).expect("bin");
        let own = std::env::current_exe().expect("this executable");
        let own = own.display().to_string();
        assert_eq!(bins(&own, &["code", "x.rs"]), [bin(&a), bin(&b)]);
        assert!(bins(&own, &["image", "x.png"]).is_empty());
        assert!(bins("sh", &["-c", "code"]).is_empty());
        let rust = b.join("bin/rust").display().to_string();
        assert_eq!(bins(&rust, &[]), [bin(&b)]);
    }
}
