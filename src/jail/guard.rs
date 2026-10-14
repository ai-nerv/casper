//! The Landlock path's stand-in for bubblewrap's read-only binds beneath a writable root.
//!
//! Each protected path is read before a command runs and put back once it has ended: what changed
//! is restored, what appeared is removed, what went is recreated. Nothing is followed through a
//! link, and a restored file is written beside its place and renamed into it.

use rustix::fs::{AtFlags, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// The most file content one protected path may hold, a directory's files together.
const MOST: usize = 1024 * 1024;

/// The most entries one protected path may hold.
const ENTRIES: usize = 256;

/// How deep a protected directory may nest.
const DEPTH: usize = 8;

/// How deep a removal goes.
const DEEPEST: usize = 64;

/// What one path held, read without following a link.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Absent,
    File {
        bytes: Vec<u8>,
        mode: u32,
    },
    Link(OsString),
    Dir {
        mode: u32,
        entries: BTreeMap<OsString, State>,
    },
    /// Past the limits, unreadable, or neither file, link nor directory.
    Other,
}

/// One protected path: its writable root, its name beneath it, how a result names it, what it held.
struct Kept {
    root: PathBuf,
    name: &'static str,
    shown: String,
    was: State,
}

/// Every protected path beneath the writable roots, as it stood before a command.
pub(super) struct Guard(Vec<Kept>);

impl Guard {
    /// Read each `(root, name)`, or refuse naming the one that cannot be kept.
    pub(super) fn take<'a>(
        cwd: &Path,
        paths: impl Iterator<Item = (&'a Path, &'static str)>,
    ) -> std::io::Result<Self> {
        let cwd = std::fs::canonicalize(cwd)?;
        let mut kept: Vec<Kept> = Vec::new();
        for (root, name) in paths {
            let root = std::fs::canonicalize(root)?;
            if kept.iter().any(|k| k.root == root && k.name == name) {
                continue;
            }
            let shown = root
                .strip_prefix(&cwd)
                .map_or_else(|_| root.join(name), |within| within.join(name))
                .display()
                .to_string();
            let was = read(&root, name, &shown)?;
            kept.push(Kept {
                root,
                name,
                shown,
                was,
            });
        }
        Ok(Self(kept))
    }

    /// Put back whatever a command changed, and say what. Safe to repeat.
    pub(super) fn restore(&self) -> Vec<String> {
        let mut said = Vec::new();
        for kept in &self.0 {
            if let Err(why) = put_back(kept, &mut said) {
                said.push(format!("could not restore {}: {why}", kept.shown));
            }
        }
        for line in &said {
            crate::noted!("jail: {line}");
        }
        said
    }
}

/// The directories on the way to `name`, and its last component.
fn split(name: &'static str) -> (impl Iterator<Item = &'static OsStr>, &'static OsStr) {
    let (parents, last) = name.rsplit_once('/').unwrap_or(("", name));
    let parents = parents.split('/').filter(|p| !p.is_empty()).map(OsStr::new);
    (parents, OsStr::new(last))
}

/// A writable root, reached without a symbolic link anywhere on its path.
fn opened_root(root: &Path) -> std::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let fd = rustix::fs::openat2(
        rustix::fs::CWD,
        root,
        flags,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )?;
    Ok(fd)
}

/// The type of `name` in `dir`, unfollowed; `None` when nothing is there.
fn kind(dir: BorrowedFd<'_>, name: &OsStr) -> rustix::io::Result<Option<FileType>> {
    match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
        Err(Errno::NOENT) => Ok(None),
        Err(why) => Err(why),
    }
}

/// What `name` beneath `root` holds before a command, refusing what the guard cannot keep.
fn read(root: &Path, name: &'static str, shown: &str) -> std::io::Result<State> {
    let refused = |why: &str| std::io::Error::other(format!("{shown} {why}"));
    let linked =
        "is reached through a symbolic link, which a jailed command's guard does not follow";
    let (parents, last) = split(name);
    let mut dir = opened_root(root)?;
    for part in parents {
        match kind(dir.as_fd(), part)? {
            Some(FileType::Directory) => {}
            Some(FileType::Symlink) => return Err(refused(linked)),
            _ => return Ok(State::Absent),
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        dir = rustix::fs::openat(&dir, part, flags, Mode::empty())?;
    }
    let mut budget = Budget::default();
    let state = state(dir.as_fd(), last, &mut budget, 0)?;
    if budget.over {
        return Err(refused(&format!(
            "holds more than the {MOST} bytes or {ENTRIES} entries a jailed command's guard keeps"
        )));
    }
    if matches!(state, State::Link(_)) {
        return Err(refused(linked));
    }
    if !whole(&state) {
        return Err(refused(
            "holds something other than files, links and directories, which the guard cannot keep",
        ));
    }
    Ok(state)
}

/// Whether nothing in `state` is [`State::Other`].
fn whole(state: &State) -> bool {
    match state {
        State::Other => false,
        State::Dir { entries, .. } => entries.values().all(whole),
        _ => true,
    }
}

/// How much one read has taken, and whether it went past the limits.
#[derive(Default)]
struct Budget {
    bytes: usize,
    entries: usize,
    over: bool,
}

/// What `name` in `dir` holds, never followed through a link.
fn state(
    dir: BorrowedFd<'_>,
    name: &OsStr,
    budget: &mut Budget,
    depth: usize,
) -> std::io::Result<State> {
    let Some(kind) = kind(dir, name)? else {
        return Ok(State::Absent);
    };
    budget.entries += 1;
    if budget.entries > ENTRIES || depth > DEPTH {
        budget.over = true;
        return Ok(State::Other);
    }
    match kind {
        FileType::RegularFile => file(dir, name, budget),
        FileType::Symlink => {
            let target = rustix::fs::readlinkat(dir, name, Vec::new())?;
            Ok(State::Link(OsString::from_vec(target.into_bytes())))
        }
        FileType::Directory => {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fd = rustix::fs::openat(dir, name, flags, Mode::empty())?;
            let mode = rustix::fs::fstat(&fd)?.st_mode & 0o7777;
            let mut entries = BTreeMap::new();
            for entry in names(&fd)? {
                let held = state(fd.as_fd(), &entry, budget, depth + 1)?;
                if budget.over {
                    return Ok(State::Other);
                }
                entries.insert(entry, held);
            }
            Ok(State::Dir { mode, entries })
        }
        _ => Ok(State::Other),
    }
}

/// A regular file's bytes and mode, or [`State::Other`] past what the budget has left.
fn file(dir: BorrowedFd<'_>, name: &OsStr, budget: &mut Budget) -> std::io::Result<State> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(dir, name, flags, Mode::empty())?;
    let stat = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Ok(State::Other);
    }
    let room = MOST.saturating_sub(budget.bytes);
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match rustix::io::read(&fd, &mut chunk) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(chunk.get(..read).unwrap_or_default()),
            Err(Errno::INTR) => continue,
            Err(why) => return Err(why.into()),
        }
        if bytes.len() > room {
            budget.over = true;
            return Ok(State::Other);
        }
    }
    budget.bytes += bytes.len();
    let mode = stat.st_mode & 0o7777;
    Ok(State::File { bytes, mode })
}

/// Up to [`ENTRIES`] + 1 names in a directory, `.` and `..` aside.
fn names(dir: &OwnedFd) -> std::io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir)? {
        let name = OsStr::from_bytes(entry?.file_name().to_bytes()).to_owned();
        if name != "." && name != ".." {
            names.push(name);
        }
        if names.len() > ENTRIES {
            break;
        }
    }
    Ok(names)
}

/// `name` in `dir` as a directory this process may change, opened without following a link.
fn opened_dir(dir: BorrowedFd<'_>, name: &OsStr) -> std::io::Result<OwnedFd> {
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let held = rustix::fs::openat(dir, name, flags, Mode::empty())?;
    let at = format!("/proc/self/fd/{}", held.as_raw_fd());
    let mode = Mode::from_raw_mode(rustix::fs::fstat(&held)?.st_mode);
    if !mode.contains(Mode::RWXU) {
        rustix::fs::chmod(at.as_str(), mode | Mode::RWXU)?;
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Ok(rustix::fs::open(at.as_str(), flags, Mode::empty())?)
}

/// Make the protected path hold what it held before the command.
fn put_back(kept: &Kept, said: &mut Vec<String>) -> std::io::Result<()> {
    let absent = kept.was == State::Absent;
    let base = kept.shown.strip_suffix(kept.name).unwrap_or_default();
    let (parents, last) = split(kept.name);
    let mut walked = PathBuf::new();
    let mut dir = opened_root(&kept.root)?;
    for part in parents {
        walked.push(part);
        match kind(dir.as_fd(), part)? {
            Some(FileType::Directory) => {}
            None if absent => return Ok(()),
            None => rustix::fs::mkdirat(&dir, part, Mode::from_raw_mode(0o755))?,
            Some(_) if !absent => {
                remove(dir.as_fd(), part, 0)?;
                rustix::fs::mkdirat(&dir, part, Mode::from_raw_mode(0o755))?;
            }
            Some(FileType::Symlink) => {
                remove(dir.as_fd(), part, 0)?;
                let shown = format!("{base}{}", walked.display());
                said.push(notice(&State::Absent, &shown));
                return Ok(());
            }
            Some(_) => return Ok(()),
        }
        dir = opened_dir(dir.as_fd(), part)?;
    }
    let now = state(dir.as_fd(), last, &mut Budget::default(), 0).unwrap_or(State::Other);
    settle(dir.as_fd(), last, &kept.was, &now, &kept.shown, said)
}

/// What a result says of one path put back.
fn notice(was: &State, shown: &str) -> String {
    if *was == State::Absent {
        format!("removed {shown}: a jailed command may not create it")
    } else {
        format!("restored {shown}: a jailed command may not change it")
    }
}

/// Make `name` in `dir` hold `was` again, given it holds `now`; a directory entry by entry.
fn settle(
    dir: BorrowedFd<'_>,
    name: &OsStr,
    was: &State,
    now: &State,
    shown: &str,
    said: &mut Vec<String>,
) -> std::io::Result<()> {
    if was == now {
        return Ok(());
    }
    let (
        State::Dir { mode, entries },
        State::Dir {
            mode: held,
            entries: found,
        },
    ) = (was, now)
    else {
        replace(dir, name, was, now)?;
        said.push(notice(was, shown));
        return Ok(());
    };
    let inner = opened_dir(dir, name)?;
    let every: BTreeSet<&OsString> = entries.keys().chain(found.keys()).collect();
    for entry in every {
        let shown = format!("{shown}/{}", entry.to_string_lossy());
        let was = entries.get(entry).unwrap_or(&State::Absent);
        let now = found.get(entry).unwrap_or(&State::Absent);
        if let Err(why) = settle(inner.as_fd(), entry, was, now, &shown, said) {
            said.push(format!("could not restore {shown}: {why}"));
        }
    }
    rustix::fs::fchmod(&inner, Mode::from_raw_mode(*mode))?;
    if mode != held {
        said.push(notice(was, shown));
    }
    Ok(())
}

/// Put `was` at `name` in `dir` in place of `now`: a file or link beside it and renamed over it.
fn replace(dir: BorrowedFd<'_>, name: &OsStr, was: &State, now: &State) -> std::io::Result<()> {
    let cleared = matches!(now, State::Dir { .. } | State::Other)
        || matches!(was, State::Absent | State::Dir { .. });
    if cleared {
        remove(dir, name, 0)?;
    }
    let temp = match was {
        State::Absent | State::Other => return Ok(()),
        State::Dir { mode, entries } => {
            rustix::fs::mkdirat(dir, name, Mode::RWXU)?;
            let inner = opened_dir(dir, name)?;
            for (entry, state) in entries {
                replace(inner.as_fd(), entry, state, &State::Absent)?;
            }
            rustix::fs::fchmod(&inner, Mode::from_raw_mode(*mode))?;
            return Ok(());
        }
        State::File { bytes, mode } => file_beside(dir, name, bytes, *mode)?,
        State::Link(target) => {
            let link = |temp: &OsStr| rustix::fs::symlinkat(target.as_os_str(), dir, temp);
            beside(name, link)?.0
        }
    };
    rustix::fs::renameat(dir, &temp, dir, name).map_err(|why| {
        let _ = rustix::fs::unlinkat(dir, &temp, AtFlags::empty());
        why.into()
    })
}

/// A fresh name beside `name`, taken by `make`, which fails with `EEXIST` when it is in use.
fn beside<T>(
    name: &OsStr,
    make: impl Fn(&OsStr) -> rustix::io::Result<T>,
) -> std::io::Result<(OsString, T)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..16 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temp = OsString::from(".");
        temp.push(name);
        temp.push(format!(".casper-{}-{n}", std::process::id()));
        match make(&temp) {
            Ok(made) => return Ok((temp, made)),
            Err(Errno::EXIST) => {}
            Err(why) => return Err(why.into()),
        }
    }
    Err(std::io::Error::other("no free name beside it"))
}

/// `bytes` with `mode` in a new file beside `name`, flushed to disk; answers its name.
fn file_beside(
    dir: BorrowedFd<'_>,
    name: &OsStr,
    bytes: &[u8],
    mode: u32,
) -> std::io::Result<OsString> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let (temp, fd) = beside(name, |temp| {
        rustix::fs::openat(dir, temp, flags, Mode::RUSR | Mode::WUSR)
    })?;
    let written = write_all(&fd, bytes)
        .and_then(|()| rustix::fs::fchmod(&fd, Mode::from_raw_mode(mode)))
        .and_then(|()| rustix::fs::fsync(&fd));
    if let Err(why) = written {
        let _ = rustix::fs::unlinkat(dir, &temp, AtFlags::empty());
        return Err(why.into());
    }
    Ok(temp)
}

fn write_all(fd: &OwnedFd, mut bytes: &[u8]) -> rustix::io::Result<()> {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(Errno::IO),
            Ok(wrote) => bytes = bytes.get(wrote..).unwrap_or_default(),
            Err(Errno::INTR) => {}
            Err(why) => return Err(why),
        }
    }
    Ok(())
}

/// Remove `name` from `dir` whatever it is, a directory with everything beneath it.
fn remove(dir: BorrowedFd<'_>, name: &OsStr, depth: usize) -> std::io::Result<()> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => return Ok(()),
        Err(Errno::ISDIR | Errno::PERM) => {}
        Err(why) => return Err(why.into()),
    }
    if depth > DEEPEST {
        return Err(std::io::Error::other("nested too deeply to remove"));
    }
    let inner = opened_dir(dir, name)?;
    loop {
        let found = names(&inner)?;
        if found.is_empty() {
            break;
        }
        for entry in found {
            remove(inner.as_fd(), &entry, depth + 1)?;
        }
    }
    rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::os::unix::fs::PermissionsExt;

    /// A checkout with a hook, a git config and a project config, each `ORIGINAL`.
    fn checkout(name: &str) -> (Scratch, PathBuf) {
        let dir = Scratch::new("guard", name);
        let work = dir.join("work");
        std::fs::create_dir_all(work.join(".git/hooks")).expect("repository");
        for file in [".magi.lua", ".git/config", ".git/hooks/pre-commit"] {
            std::fs::write(work.join(file), "ORIGINAL\n").expect("protected file");
        }
        (dir, work)
    }

    fn guarded(work: &Path) -> Guard {
        let grants = super::super::Grants::default();
        Guard::take(work, super::super::persistent(work, &grants)).expect("kept")
    }

    fn original(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
            && std::fs::read_to_string(path).ok().as_deref() == Some("ORIGINAL\n")
    }

    fn listed(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn what_a_command_changed_created_or_removed_is_put_back_and_named() {
        let (dir, work) = checkout("changed");
        let outside = dir.join("outside");
        std::fs::write(&outside, "OUTSIDE\n").expect("outside");
        let guard = guarded(&work);
        std::fs::write(work.join(".magi.lua"), "bad\n").expect("modify");
        std::fs::remove_file(work.join(".git/config")).expect("delete");
        std::fs::write(work.join(".balthasar.lua"), "bad\n").expect("create");
        std::fs::remove_file(work.join(".git/hooks/pre-commit")).expect("unlink");
        std::os::unix::fs::symlink(&outside, work.join(".git/hooks/pre-commit")).expect("link");
        std::fs::write(work.join(".git/hooks/post-checkout"), "bad\n").expect("new hook");
        let said = guard.restore().join("\n");
        for file in [".magi.lua", ".git/config", ".git/hooks/pre-commit"] {
            assert!(original(&work.join(file)), "{file}: {said}");
            let line = format!("restored {file}: a jailed command may not change it");
            assert!(said.contains(&line), "{line}: {said}");
        }
        for file in [".balthasar.lua", ".git/hooks/post-checkout"] {
            assert!(!work.join(file).exists(), "{file}");
            let line = format!("removed {file}: a jailed command may not create it");
            assert!(said.contains(&line), "{line}: {said}");
        }
        assert_eq!(
            std::fs::read_to_string(&outside).expect("outside"),
            "OUTSIDE\n"
        );
        assert_eq!(listed(&work), [".git", ".magi.lua"], "nothing left beside");
        assert_eq!(listed(&work.join(".git/hooks")), ["pre-commit"]);
        assert!(
            guard.restore().is_empty(),
            "a second restore has nothing to do"
        );
    }

    #[test]
    fn a_file_turned_into_a_directory_or_reached_through_a_link_is_a_file_again() {
        let (dir, work) = checkout("retyped");
        let config = work.join(".git/config");
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).expect("mode");
        let guard = guarded(&work);
        std::fs::remove_file(work.join(".magi.lua")).expect("unlink");
        std::fs::create_dir_all(work.join(".magi.lua/sealed")).expect("directory");
        std::fs::write(work.join(".magi.lua/sealed/x"), "bad\n").expect("nested");
        let sealed = std::fs::Permissions::from_mode(0o000);
        std::fs::set_permissions(work.join(".magi.lua/sealed"), sealed).expect("sealed");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        std::fs::rename(work.join(".git"), dir.join("moved")).expect("move");
        std::os::unix::fs::symlink(&elsewhere, work.join(".git")).expect("link");
        let said = guard.restore().join("\n");
        assert!(original(&work.join(".magi.lua")), "{said}");
        assert!(
            std::fs::symlink_metadata(work.join(".git"))
                .expect(".git")
                .is_dir()
        );
        assert!(original(&config) && original(&work.join(".git/hooks/pre-commit")));
        let mode = std::fs::metadata(&config)
            .expect("config")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o640, "the mode is put back too");
        assert!(
            listed(&elsewhere).is_empty(),
            "nothing was written through the link"
        );
    }

    #[test]
    fn a_repository_made_where_there_was_none_loses_what_the_jail_keeps() {
        let dir = Scratch::new("guard", "fresh");
        let work = dir.join("work");
        std::fs::create_dir(&work).expect("work");
        let guard = guarded(&work);
        std::fs::create_dir_all(work.join(".git/hooks")).expect("init");
        for file in [".git/config", ".git/HEAD", ".git/hooks/pre-commit"] {
            std::fs::write(work.join(file), "bad\n").expect("init");
        }
        let said = guard.restore().join("\n");
        assert_eq!(listed(&work.join(".git")), ["HEAD"], "{said}");
        assert!(said.contains("removed .git/hooks: a jailed command may not create it"));
        assert!(said.contains("removed .git/config: a jailed command may not create it"));
    }

    #[test]
    fn a_protected_file_past_the_cap_or_behind_a_link_refuses_the_command() {
        let (dir, work) = checkout("refused");
        std::fs::write(work.join(".magi.lua"), vec![b'x'; MOST + 1]).expect("large");
        let grants = super::super::Grants::default();
        let why = Guard::take(&work, super::super::persistent(&work, &grants)).err();
        let why = why.map(|why| why.to_string()).unwrap_or_default();
        assert!(why.starts_with(".magi.lua holds more than"), "{why}");
        std::fs::write(work.join(".magi.lua"), "ORIGINAL\n").expect("small");
        std::fs::write(dir.join("shared.lua"), "x").expect("target");
        std::os::unix::fs::symlink(dir.join("shared.lua"), work.join(".balthasar.lua"))
            .expect("link");
        let why = Guard::take(&work, super::super::persistent(&work, &grants)).err();
        let why = why.map(|why| why.to_string()).unwrap_or_default();
        assert!(
            why.contains(".balthasar.lua is reached through a symbolic link"),
            "{why}"
        );
    }
}
