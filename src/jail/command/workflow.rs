use super::super::{filter, policy};
use super::Prepared;
use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr,
    RulesetCreatedAttr, Scope,
};
use rustix::fs::{self, Mode, OFlags, ResolveFlags};
use std::io::{Error, Result};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

pub(crate) const WORKFLOW_BINDING: &str = "CASPER_WORKFLOW_BINDING";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowBinding {
    pub(crate) root: PathBuf,
    device: u64,
    inode: u64,
}

impl WorkflowBinding {
    pub(crate) fn of(root: &Path, fd: &OwnedFd) -> Result<Self> {
        let stat = fs::fstat(fd)?;
        Ok(Self {
            root: root.into(),
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }

    pub(crate) fn matches(&self, root: &Path, fd: &OwnedFd) -> bool {
        fs::fstat(fd).is_ok_and(|stat| {
            self.root == root && self.device == stat.st_dev && self.inode == stat.st_ino
        })
    }
}

pub fn workflow(root: &Path) -> Result<Prepared> {
    prepare(
        root,
        &std::env::current_exe()?,
        &["workflows".into(), "--worker".into()],
        super::super::which("bwrap").as_deref().map(Path::new),
    )
}

fn prepare(
    root: &Path,
    program: &Path,
    args: &[String],
    launcher: Option<&Path>,
) -> Result<Prepared> {
    if !root.is_absolute()
        || root.as_os_str().as_bytes().len() > 4096
        || root.components().count() > 256
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(Error::other("workflow root must be bounded and absolute"));
    }
    let root_fd = fs::openat2(
        fs::CWD,
        root,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )?;
    let identity = WorkflowBinding::of(root, &root_fd)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let protected = policy::protected(&home)?;
    let mut runtime = Vec::new();
    for path in [
        "/usr",
        "/lib",
        "/lib64",
        "/bin",
        "/nix/store",
        "/etc/ld.so.cache",
    ] {
        let path = PathBuf::from(path);
        if path.exists() {
            runtime.push((path.clone(), policy::validated_fd(&path, &protected)?));
        }
    }
    runtime.push((
        program.to_owned(),
        policy::validated_fd(program, &protected)?,
    ));
    let filters = vec![
        super::super::filter(false),
        filter::families(false),
        super::super::absent(),
    ];
    if filters.iter().any(Vec::is_empty) {
        return Err(Error::other("workflow seccomp unavailable"));
    }
    let mut prepared = Prepared {
        program: program.to_owned(),
        args: args.to_vec(),
        cwd: PathBuf::from("/"),
        env: vec![
            ("HOME".into(), "/nonexistent".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("CASPER_WORKFLOW_WORKER".into(), "1".into()),
            (
                WORKFLOW_BINDING.into(),
                serde_json::to_string(&identity)
                    .map_err(Error::other)?
                    .into(),
            ),
        ],
        clear: true,
        filters: Vec::new(),
        landlock: None,
        descriptors: Vec::new(),
        guard: None,
        separator: None,
        _temporary: None,
    };
    if let Some(launcher) = launcher {
        let launcher = policy::launcher(launcher, &protected)?;
        let mut wrapped = Vec::new();
        for (path, fd) in runtime
            .into_iter()
            .chain(std::iter::once((root.to_owned(), root_fd)))
        {
            wrapped.extend([
                "--ro-bind-fd".into(),
                fd.as_raw_fd().to_string(),
                utf8(&path)?,
            ]);
            prepared.descriptors.push(fd);
        }
        wrapped.extend(
            [
                "--unshare-net",
                "--unshare-pid",
                "--unshare-ipc",
                "--unshare-uts",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--chdir",
                "/",
            ]
            .map(str::to_owned),
        );
        for filter in filters {
            let fd = filter::descriptor(&filter)?;
            wrapped.extend(["--add-seccomp-fd".into(), fd.as_raw_fd().to_string()]);
            prepared.descriptors.push(fd);
        }
        wrapped.extend(["--remount-ro".into(), "/".into()]);
        prepared.separator = Some(wrapped.len());
        wrapped.extend(["--".into(), utf8(program)?]);
        wrapped.extend(args.iter().cloned());
        prepared.landlock = Some(
            super::super::scoped()
                .ok_or_else(|| Error::other("workflow socket scoping unavailable"))?,
        );
        prepared.program = launcher;
        prepared.args = wrapped;
    } else {
        prepared.landlock = Some(read_only(root_fd, runtime)?);
        prepared.filters = filters;
    }
    Ok(prepared)
}

fn read_only(root: OwnedFd, runtime: Vec<(PathBuf, OwnedFd)>) -> Result<landlock::RulesetCreated> {
    let abi = ABI::V6;
    let mut rules = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(unavailable)?
        .handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp)
        .map_err(unavailable)?
        .scope(Scope::Signal | Scope::AbstractUnixSocket)
        .map_err(unavailable)?
        .create()
        .map_err(unavailable)?;
    rules = rules
        .add_rule(PathBeneath::new(
            root,
            AccessFs::ReadFile | AccessFs::ReadDir,
        ))
        .map_err(unavailable)?;
    for (_, fd) in runtime {
        let stat = fs::fstat(&fd)?;
        let access = if fs::FileType::from_raw_mode(stat.st_mode) == fs::FileType::Directory {
            AccessFs::from_read(abi)
        } else {
            AccessFs::ReadFile | AccessFs::Execute
        };
        rules = rules
            .add_rule(PathBeneath::new(fd, access))
            .map_err(unavailable)?;
    }
    Ok(rules)
}

fn unavailable<T>(_: T) -> Error {
    Error::other("workflow read-only Landlock unavailable")
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::other("workflow path must be UTF-8"))
}

#[cfg(test)]
mod tests;
