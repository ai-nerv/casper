//! casper's socket door: the same `tools` and `run`, on a socket a coordinator holds open across a
//! session's calls. The trust is not the caller's — `casper serve` is spawned with the session's
//! jail in its environment ([`crate::jail`]), so every call runs inside walls the coordinator set
//! at spawn, never ones a caller named, and a peer of another user is turned away. This is why the
//! socket is not a remote shell: what `run` runs, it runs jailed, and the jail is the spawner's.

use crate::framing::{self, Wire};
use crate::wire::{Call, Reply};
use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

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

/// Answer on `listener` until it closes, one connection at a time — casper's calls are serial — and
/// reply to each in the call's own encoding. A single bad connection is skipped, not fatal.
pub fn accept(listener: &UnixListener, handle: impl Fn(&Call) -> Reply) -> std::io::Result<()> {
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        if !ours(&stream) {
            continue;
        }
        if let Err(why) = talk(&stream, &handle) {
            crate::noted!("serve: a connection ended badly: {why}");
        }
    }
    Ok(())
}

/// One connection: read a call, answer it, repeat until the peer closes.
fn talk(stream: &UnixStream, handle: &impl Fn(&Call) -> Reply) -> std::io::Result<()> {
    let mut reader = std::io::BufReader::new(stream);
    loop {
        let body = match framing::read_frame(&mut reader) {
            Ok(body) => body,
            Err(why) if why.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(why) => return Err(why),
        };
        let wire = Wire::of(&body);
        let reply = match framing::decode::<Call>(&body) {
            Ok(call) => handle(&call),
            Err(why) => Reply::refused(format!("that is not a call: {why}")),
        };
        framing::write_frame(&mut { stream }, &wire.encode(&reply)?)?;
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
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_the_runtime_is_not_one_to_bind() {
        assert!(!inside(Path::new("/tmp/casper.sock")));
        assert!(!inside(&runtime().join("../escape")));
        assert!(inside(&runtime().join("proj/id")));
    }

    #[test]
    fn a_call_over_the_socket_is_answered_in_its_own_encoding() {
        // The bind guard wants a path under the runtime. The `casper` dir is removed afterwards only
        // when the test made it, so a developer's own live sockets are never swept up.
        let base = runtime();
        let ours = !base.exists();
        let at = base.join(format!("t-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&at);
        let listener = listening_on(&at).expect("bind");
        let path = at.clone();
        let server = std::thread::spawn(move || {
            accept(&listener, |call| {
                Reply::of(serde_json::json!({ "echoed": call.call }))
            })
        });

        let mut client = UnixStream::connect(&path).expect("connect");
        let body = Wire::Json
            .encode(&serde_json::json!({ "call": "tools", "args": [] }))
            .expect("encode");
        framing::write_frame(&mut client, &body).expect("send");
        let got = framing::read_frame(&mut client).expect("recv");
        let reply: Reply = framing::decode(&got).expect("decode");
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.result[0]["echoed"], "tools");

        drop(client);
        drop(server);
        let _ = std::fs::remove_file(&path);
        if ours {
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}
