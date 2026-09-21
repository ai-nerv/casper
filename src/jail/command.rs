//! A captured jail policy shared by ordinary and terminal children.

use super::temporary::Temporary;
use super::{Jail, policy};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub struct Prepared {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    clear: bool,
    filter: Option<seccompiler::BpfProgram>,
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
            filter: None,
            landlock: None,
            descriptors: Vec::new(),
            _temporary: None,
        };
        let Some(grants) = &self.grants else {
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
        let mut forbidden = super::FORBIDDEN.to_vec();
        if !grants.reach {
            forbidden.extend([
                libc::SYS_socket,
                libc::SYS_connect,
                libc::SYS_sendto,
                libc::SYS_sendmsg,
                libc::SYS_sendmmsg,
            ]);
        }
        let filter = super::deny(&forbidden);
        if filter.is_empty() {
            return Err(std::io::Error::other("required seccomp filter unavailable"));
        }
        if let Some(bwrap) = bwrap {
            use std::os::fd::AsRawFd;
            let bwrap = policy::launcher(Path::new(&bwrap), &protected)?;
            let descriptor = super::filter::descriptor(&filter)?;
            let mut wrapped = super::profile(&self.cwd, grants);
            prepared.descriptors = policy::bindings(&mut wrapped, &protected, &bwrap)?;
            wrapped.truncate(wrapped.len() - 2);
            if terminal {
                wrapped.retain(|arg| arg != "--new-session");
            }
            wrapped.extend(["--chdir".into(), prepared.cwd.display().to_string()]);
            wrapped.extend(["--seccomp".into(), descriptor.as_raw_fd().to_string()]);
            wrapped.push("--".into());
            wrapped.push(program.into());
            wrapped.extend(args.iter().cloned());
            prepared.program = bwrap;
            prepared.args = wrapped;
            prepared.descriptors.push(descriptor);
        } else {
            prepared.filter = Some(filter);
            let mut granted = grants.clone();
            granted.tmp = Some(tmp);
            policy::validate(&self.cwd, &granted, &protected)?;
            prepared.landlock = Some(super::ruleset(&self.cwd, &granted, &protected).ok_or_else(
                || std::io::Error::other("required Landlock protection unavailable"),
            )?);
        }
        Ok(prepared)
    }
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
        if let Some(filter) = self.filter.take() {
            crate::tied::confine(&mut command, filter);
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
            self.filter.take(),
            self.landlock.take(),
            std::mem::take(&mut self.descriptors),
        )
    }
}
