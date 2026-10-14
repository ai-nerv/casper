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
    use std::os::unix::fs::MetadataExt;
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?);
    let meta = std::fs::metadata(&dir).ok()?;
    (dir.is_absolute()
        && meta.is_dir()
        && meta.uid() == rustix::process::getuid().as_raw()
        && meta.mode() & 0o077 == 0)
        .then_some(dir)
}

/// Where casper's sockets live: `$XDG_RUNTIME_DIR/casper`, else a per-user directory under the
/// temporary directory, so two users never collide on one path.
#[must_use]
pub fn runtime() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("casper-{}", rustix::process::getuid().as_raw()))
        });
    base.join("casper")
}

/// Whether `path` is one casper may bind: under [`runtime`], and with no `..` climbing out of it.
#[must_use]
pub fn inside(path: &Path) -> bool {
    path.starts_with(runtime())
        && !path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
}

/// Bind the socket at `at`, clearing a crash's leftover first. Refuses a path outside [`runtime`].
pub fn listening_on(at: &Path) -> std::io::Result<UnixListener> {
    if !inside(at) {
        return Err(std::io::Error::other(format!(
            "{} is not under {}",
            at.display(),
            runtime().display()
        )));
    }
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket file left by a killed daemon refuses a fresh bind; one nothing answers on is cleared.
    if std::fs::metadata(at).is_ok() && UnixStream::connect(at).is_err() {
        std::fs::remove_file(at)?;
    }
    UnixListener::bind(at)
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
