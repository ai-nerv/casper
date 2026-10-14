//! casper's socket door: the same `tools` and `run`, on a socket a coordinator holds open across a
//! session's calls. The trust is not the caller's — `casper serve` is spawned with the session's
//! jail in its environment ([`crate::jail`]), so every call runs inside walls the coordinator set
//! at spawn, never ones a caller named, and a peer of another user is turned away. This is why the
//! socket is not a remote shell: what `run` runs, it runs jailed, and the jail is the spawner's.

use crate::wire::{Call, Reply};
use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

mod key;
mod talking;
#[cfg(test)]
mod tests;

pub use key::Key;

/// `$XDG_RUNTIME_DIR`, when it is an absolute path to a directory this uid owns with no group or
/// other permissions. Empty is unset.
#[must_use]
pub fn trusted_runtime() -> Option<PathBuf> {
    trusted(std::env::var_os("XDG_RUNTIME_DIR"))
}

/// [`trusted_runtime`] for a given value.
fn trusted(wanted: Option<std::ffi::OsString>) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dir = PathBuf::from(wanted.filter(|v| !v.is_empty())?);
    let meta = std::fs::metadata(&dir).ok()?;
    (dir.is_absolute() && meta.is_dir() && meta.uid() == uid() && meta.mode() & 0o077 == 0)
        .then_some(dir)
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Where casper's sockets live: `$XDG_RUNTIME_DIR/casper` when that is trusted, else
/// `<tmp>/casper-<uid>`.
#[must_use]
pub fn runtime() -> PathBuf {
    trusted_runtime().map_or_else(
        || std::env::temp_dir().join(format!("casper-{}", uid())),
        |dir| dir.join("casper"),
    )
}

/// Whether `path` is one casper may bind: beneath [`runtime`], and with no `.` or `..` in it.
#[must_use]
pub fn inside(path: &Path) -> bool {
    within(&runtime(), path)
}

fn within(root: &Path, path: &Path) -> bool {
    use std::path::Component;
    path.starts_with(root)
        && path != root
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// A bound socket, the lock held for as long as it listens, and the inode it was bound as: on
/// drop the path is unlinked only while it is still that inode.
pub struct Listening {
    pub listener: UnixListener,
    path: PathBuf,
    bound: (u64, u64),
    _lock: std::fs::File,
}

impl Drop for Listening {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        let ours = std::fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| (meta.dev(), meta.ino()) == self.bound);
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Bind the socket at `at`, beneath [`runtime`].
///
/// # Errors
/// See [`listening_in`].
pub fn listening_on(at: &Path) -> std::io::Result<Listening> {
    listening_in(&runtime(), at)
}

/// Bind the socket at `at` beneath `root`. Every directory from `root` down is made `0700` and
/// must then be a real directory of this user's with no group or other permissions. The bind
/// holds an exclusive lock on `<at>.lock`, and a path already there is unlinked only when it is a
/// socket of this user's that refuses a connection.
///
/// # Errors
/// A path outside `root`, a directory that is not private, a lock another holds, or a path that
/// is not a stale socket of this user's.
pub fn listening_in(root: &Path, at: &Path) -> std::io::Result<Listening> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
    let refused = |why: String| std::io::Error::other(why);
    let parent = at.parent().filter(|_| within(root, at));
    let Some(parent) = parent else {
        return Err(refused(format!(
            "{} is not under {}",
            at.display(),
            root.display()
        )));
    };
    private(root)?;
    let mut dir = root.to_path_buf();
    for part in parent.strip_prefix(root).map_err(std::io::Error::other)? {
        dir.push(part);
        private(&dir)?;
    }
    let mut named = at.as_os_str().to_owned();
    named.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&named)?;
    let exclusive = rustix::fs::FlockOperation::NonBlockingLockExclusive;
    if rustix::fs::flock(&lock, exclusive).is_err() {
        return Err(refused(format!(
            "another serve holds {}",
            Path::new(&named).display()
        )));
    }
    match std::fs::symlink_metadata(at) {
        Ok(meta) if !meta.file_type().is_socket() || meta.uid() != uid() => {
            return Err(refused(format!(
                "{} is not a socket of this user's, and is left where it is",
                at.display()
            )));
        }
        Ok(_) if !stale(at) => {
            return Err(refused(format!(
                "{} is answered by a live program",
                at.display()
            )));
        }
        Ok(_) => std::fs::remove_file(at)?,
        Err(why) if why.kind() == ErrorKind::NotFound => {}
        Err(why) => return Err(why),
    }
    let listener = UnixListener::bind(at)?;
    let meta = std::fs::symlink_metadata(at)?;
    Ok(Listening {
        listener,
        path: at.to_path_buf(),
        bound: (meta.dev(), meta.ino()),
        _lock: lock,
    })
}

/// Make `dir` with mode `0700` if it is not there, then require it to be a directory of this
/// user's, not a link, with no group or other permissions.
fn private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(why) if why.kind() == ErrorKind::AlreadyExists => {}
        Err(why) => return Err(why),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "{} is not a private directory of this user's",
            dir.display()
        )));
    }
    Ok(())
}

/// Whether nothing answers at `at`: a connection refused outright. Anything else, a full
/// backlog or an accepted connection, is a live program.
fn stale(at: &Path) -> bool {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let flags = SocketFlags::NONBLOCK | SocketFlags::CLOEXEC;
    let Ok(socket) = rustix::net::socket_with(AddressFamily::UNIX, SocketType::STREAM, flags, None)
    else {
        return false;
    };
    let Ok(address) = SocketAddrUnix::new(at) else {
        return false;
    };
    rustix::net::connect(&socket, &address) == Err(rustix::io::Errno::CONNREFUSED)
}

/// The most connections answered at once; the next is told it is busy.
pub const CONNECTIONS: usize = 8;

/// How long the loop rests after an accept that failed for want of a resource.
const BACKOFF: Duration = Duration::from_millis(100);

/// What every connection is answered with: the key and the surface.
type Shared = (Key, Box<dyn Fn(&Call) -> Reply + Send + Sync>);

/// Answer on `listener` until it fails for good: each connection on a thread of its own, a call
/// with the key `serve` was started with, and a reply in the call's own encoding.
pub fn accept(
    listener: &UnixListener,
    key: Key,
    handle: impl Fn(&Call) -> Reply + Send + Sync + 'static,
) -> std::io::Result<()> {
    accepting(|| listener.accept().map(|(stream, _)| stream), key, handle)
}

/// [`accept`] over any source of connections.
fn accepting(
    mut next: impl FnMut() -> std::io::Result<UnixStream>,
    key: Key,
    handle: impl Fn(&Call) -> Reply + Send + Sync + 'static,
) -> std::io::Result<()> {
    let shared: Arc<Shared> = Arc::new((key, Box::new(handle)));
    let open = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = match next() {
            Ok(stream) => stream,
            Err(why) if why.kind() == ErrorKind::Interrupted => continue,
            Err(why) if passing(&why) => {
                crate::noted!("serve: accept failed, resting: {why}");
                std::thread::sleep(BACKOFF);
                continue;
            }
            Err(why) => return Err(why),
        };
        if !ours(&stream) {
            continue;
        }
        if open.fetch_add(1, Ordering::SeqCst) >= CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            talking::busy(stream, CONNECTIONS);
            continue;
        }
        let slot = Slot(Arc::clone(&open));
        let shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("casper-serve".to_owned())
            .spawn(move || {
                let _slot = slot;
                let (key, handle) = &*shared;
                if let Err(why) = talking::talk(stream, key, handle.as_ref()) {
                    crate::noted!("serve: a connection ended badly: {why}");
                }
            });
        if let Err(why) = spawned {
            crate::noted!("serve: no thread for a connection: {why}");
        }
    }
}

/// Whether an accept failed for want of something that comes back: descriptors, buffers, or a
/// peer that left before it was taken.
fn passing(why: &std::io::Error) -> bool {
    matches!(
        why.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM | libc::ECONNABORTED)
    )
}

/// One of [`CONNECTIONS`], given back when its connection ends however it ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Whether the peer runs as this user. A socket that would run commands for anyone is exactly the
/// remote shell the family refuses; one that answers only its own user, with the spawner's jail, is
/// not.
#[must_use]
fn ours(stream: &UnixStream) -> bool {
    use std::os::fd::AsFd;
    rustix::net::sockopt::socket_peercred(stream.as_fd())
        .is_ok_and(|cred| cred.uid == rustix::process::getuid())
}

#[cfg(test)]
mod binding {
    use super::*;
    use crate::scratch::Scratch;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).expect("stat").mode() & 0o7777
    }

    /// A private runtime directory inside a scratch one.
    fn private_root(dir: &Scratch) -> PathBuf {
        let root = dir.join("run");
        private(&root).expect("root");
        root
    }

    /// A directory beside `dir` that group and others may read.
    fn open(dir: &Scratch) -> PathBuf {
        let open = dir.join("open");
        std::fs::create_dir(&open).expect("open dir");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        open
    }

    #[test]
    fn a_runtime_directory_is_made_private_and_one_that_is_not_is_refused() {
        let dir = Scratch::new("serving", "private");
        let made = dir.join("made");
        let bound = listening_in(&made, &made.join("p/s.sock")).expect("bind");
        assert_eq!((mode(&made), mode(&made.join("p"))), (0o700, 0o700));
        drop(bound);
        let link = dir.join("link");
        std::os::unix::fs::symlink(&made, &link).expect("link");
        for root in [open(&dir), link] {
            let why = listening_in(&root, &root.join("s.sock")).err();
            let why = why.map(|e| e.to_string()).unwrap_or_default();
            assert!(why.contains(&root.display().to_string()), "{why}");
        }
    }

    #[test]
    fn an_untrusted_runtime_variable_is_not_used() {
        let dir = Scratch::new("serving", "trusted");
        let open = open(&dir);
        std::fs::write(dir.join("file"), "x").expect("file");
        for value in [
            "",
            "relative",
            "/nonexistent-run",
            &open.display().to_string(),
        ] {
            assert_eq!(trusted(Some(value.into())), None, "{value}");
        }
        assert_eq!(trusted(Some(dir.join("file").into())), None);
        assert_eq!(trusted(None), None);
        assert_eq!(
            trusted(Some(dir.as_os_str().into())),
            Some(dir.to_path_buf())
        );
    }

    #[test]
    fn serve_at_a_path_that_is_not_a_stale_socket_removes_nothing() {
        let dir = Scratch::new("serving", "remove");
        let root = private_root(&dir);
        std::fs::write(root.join("file"), "kept").expect("file");
        std::os::unix::fs::symlink(root.join("file"), root.join("link")).expect("link");
        let _live = UnixListener::bind(root.join("live")).expect("live socket");
        for name in ["file", "link", "live"] {
            assert!(listening_in(&root, &root.join(name)).is_err(), "{name}");
        }
        let kept = std::fs::read_to_string(root.join("file")).ok();
        assert_eq!(kept.as_deref(), Some("kept"));
        let link = std::fs::symlink_metadata(root.join("link")).expect("link");
        assert!(link.file_type().is_symlink());
        assert!(
            UnixStream::connect(root.join("live")).is_ok(),
            "the live socket stays"
        );
        drop(UnixListener::bind(root.join("stale")).expect("stale socket"));
        let rebound = listening_in(&root, &root.join("stale")).expect("a stale socket is cleared");
        assert!(UnixStream::connect(root.join("stale")).is_ok());
        drop(rebound);
    }

    #[test]
    fn a_second_serve_on_the_same_path_is_turned_away_by_the_lock() {
        let dir = Scratch::new("serving", "lock");
        let root = private_root(&dir);
        let at = root.join("s.sock");
        let first = listening_in(&root, &at).expect("first");
        let why = listening_in(&root, &at).err().map(|e| e.to_string());
        assert!(why.unwrap_or_default().contains("holds"));
        assert_eq!(mode(&root.join("s.sock.lock")), 0o600);
        drop(first);
    }

    #[test]
    fn two_binds_racing_on_a_stale_socket_leave_one_listener() {
        let dir = Scratch::new("serving", "race");
        let root = private_root(&dir);
        let at = root.join("s.sock");
        drop(UnixListener::bind(&at).expect("stale socket"));
        let racers: Vec<_> = (0..4)
            .map(|_| {
                let (root, at) = (root.clone(), at.clone());
                std::thread::spawn(move || listening_in(&root, &at))
            })
            .collect();
        let bound: Vec<_> = racers.into_iter().filter_map(|r| r.join().ok()).collect();
        let refused: Vec<String> = bound
            .iter()
            .filter_map(|got| got.as_ref().err().map(ToString::to_string))
            .collect();
        assert_eq!(refused.len(), bound.len() - 1, "{refused:?}");
    }

    #[test]
    fn a_socket_is_unlinked_on_drop_only_while_it_is_the_one_bound() {
        let dir = Scratch::new("serving", "drop");
        let root = private_root(&dir);
        let at = root.join("s.sock");
        let first = listening_in(&root, &at).expect("bind");
        std::fs::remove_file(&at).expect("replaced");
        let _other = UnixListener::bind(&at).expect("another socket at the path");
        drop(first);
        assert!(at.exists(), "the replacement was unlinked");
        std::fs::remove_file(&at).expect("clear");
        drop(listening_in(&root, &at).expect("bind again"));
        assert!(!at.exists(), "its own socket stays behind");
    }
}
