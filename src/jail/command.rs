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
            prepared.landlock = Some(
                super::ruleset(&self.cwd, &granted, &protected, terminal).ok_or_else(|| {
                    std::io::Error::other("required Landlock protection unavailable")
                })?,
            );
        }
        Ok(prepared)
    }
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
