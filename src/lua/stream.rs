//! The socket primitive the client libraries need. The client carries framing and encoding in
//! plain Lua; only opening the socket is lent, as a host native rather than a VM feature.
//!
//! ```lua
//! local h = magi.stream.connect(path, timeout_ms)
//! h:send(bytes)   h:recv(n)   h:close()
//! ```

use luna::{Callback, CallbackReturn, Context, Table, Value};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// The most a single `recv` will be asked for, so a peer claiming an enormous frame cannot make
/// us allocate for it before a byte of it has arrived.
const MAX_RECV: usize = 16 * 1024 * 1024;

/// The family's socket directories, by name: `<runtime>/<name>` or `<tmp>/<name>-<uid>`.
const FAMILY: &[&str] = &["magi", "casper", "melchior", "memory", "balthasar"];

/// A connected socket, shared between the handle's methods.
type Handle = Rc<RefCell<Option<UnixStream>>>;

/// The path to dial when `path` is one of the family's sockets, and `None` otherwise.
///
/// A config file is read before anything that could ask a person exists, so the reachable set is
/// the family's own socket directories and nothing else under the runtime or temporary directory.
fn dialable(path: &Path) -> Option<PathBuf> {
    dialable_in(path, &roots())
}

/// [`dialable`] against explicit roots: the parent, canonicalised, under one of them, and the
/// path itself a socket this uid owns, not a link to one.
fn dialable_in(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let name = path.file_name()?;
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    if !roots.iter().any(|root| parent.starts_with(root)) {
        return None;
    }
    let real = parent.join(name);
    let meta = std::fs::symlink_metadata(&real).ok()?;
    (meta.file_type().is_socket() && meta.uid() == rustix::process::getuid().as_raw())
        .then_some(real)
}

/// The family's socket directories for this user, canonicalised where they exist.
fn roots() -> Vec<PathBuf> {
    let uid = rustix::process::getuid().as_raw();
    let tmp = std::env::temp_dir();
    let mut out: Vec<PathBuf> = FAMILY
        .iter()
        .map(|name| tmp.join(format!("{name}-{uid}")))
        .collect();
    if let Some(runtime) = crate::serving::trusted_runtime() {
        out.extend(FAMILY.iter().map(|name| runtime.join(name)));
    }
    out.into_iter()
        .map(|root| std::fs::canonicalize(&root).unwrap_or(root))
        .collect()
}

/// Whether the peer on `socket` runs as this user.
fn ours(socket: &UnixStream) -> bool {
    use std::os::fd::AsFd;
    rustix::net::sockopt::socket_peercred(socket.as_fd())
        .is_ok_and(|cred| cred.uid == rustix::process::getuid())
}

/// Build the `stream` table.
pub fn table<'gc>(ctx: Context<'gc>) -> Table<'gc> {
    let stream = Table::new(&ctx);
    let connect = Callback::from_fn(&ctx, |ctx, _exec, mut stack| {
        let (path, timeout_ms): (Value, Value) = stack.consume(ctx)?;
        let Value::String(path) = path else {
            stack.replace(ctx, (Value::Nil, "connect needs a path"));
            return Ok(CallbackReturn::Return);
        };
        let path = String::from_utf8_lossy(path.as_bytes()).into_owned();

        // Refused as an ordinary answer rather than a raise, so a config probing for an absent
        // sibling carries on without it.
        let Some(real) = dialable(Path::new(&path)) else {
            stack.replace(
                ctx,
                (
                    Value::Nil,
                    "only a family program's own socket, owned by this user, is this VM's to open",
                ),
            );
            return Ok(CallbackReturn::Return);
        };

        // A default rather than a wait forever: a stale socket accepts and never answers.
        let timeout = match timeout_ms {
            Value::Integer(ms) if ms > 0 => Duration::from_millis(ms as u64),
            Value::Number(ms) if ms > 0.0 => Duration::from_millis(ms as u64),
            _ => Duration::from_secs(5),
        };

        match UnixStream::connect(&real) {
            Ok(socket) if !ours(&socket) => {
                stack.replace(ctx, (Value::Nil, "the peer on that socket is another user"));
            }
            // A socket that would take no deadline is refused rather than handed over: without
            // one, a peer that accepts and never answers hangs the call forever.
            Ok(socket) => match socket
                .set_read_timeout(Some(timeout))
                .and_then(|()| socket.set_write_timeout(Some(timeout)))
            {
                Ok(()) => {
                    let handle = handle_table(ctx, Rc::new(RefCell::new(Some(socket))));
                    stack.replace(ctx, handle);
                }
                Err(e) => {
                    stack.replace(
                        ctx,
                        (
                            Value::Nil,
                            format!("this socket would take no deadline, so a peer that never answers would hang the call: {e}"),
                        ),
                    );
                }
            },
            Err(e) => {
                stack.replace(ctx, (Value::Nil, e.to_string()));
            }
        }
        Ok(CallbackReturn::Return)
    });
    stream.set(ctx, "connect", connect).ok();
    stream
}

/// A handle, as the client expects: `send`, `recv`, `close`, called with `:`.
fn handle_table<'gc>(ctx: Context<'gc>, socket: Handle) -> Table<'gc> {
    let handle = Table::new(&ctx);

    let held = Rc::clone(&socket);
    let send = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (_self, bytes): (Value, Value) = stack.consume(ctx)?;
        let Value::String(bytes) = bytes else {
            stack.replace(ctx, (Value::Nil, "send needs a string"));
            return Ok(CallbackReturn::Return);
        };
        let mut slot = held.borrow_mut();
        let Some(socket) = slot.as_mut() else {
            stack.replace(ctx, (Value::Nil, "the connection is closed"));
            return Ok(CallbackReturn::Return);
        };
        match socket
            .write_all(bytes.as_bytes())
            .and_then(|()| socket.flush())
        {
            Ok(()) => stack.replace(ctx, true),
            Err(e) => stack.replace(ctx, (Value::Nil, e.to_string())),
        }
        Ok(CallbackReturn::Return)
    });
    handle.set(ctx, "send", send).ok();

    let held = Rc::clone(&socket);
    let recv = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (_self, want): (Value, Value) = stack.consume(ctx)?;
        let want = match want {
            Value::Integer(n) if n > 0 => (n as usize).min(MAX_RECV),
            Value::Number(n) if n > 0.0 => (n as usize).min(MAX_RECV),
            _ => 0,
        };
        let mut slot = held.borrow_mut();
        let Some(socket) = slot.as_mut() else {
            stack.replace(ctx, (Value::Nil, "the connection is closed"));
            return Ok(CallbackReturn::Return);
        };
        let mut buffer = vec![0_u8; want];
        match socket.read(&mut buffer) {
            // A short read is ordinary; the client asks again. Zero means the peer hung up.
            Ok(read) => {
                buffer.truncate(read);
                let text = luna::String::from_slice(&ctx, &buffer);
                stack.replace(ctx, text);
            }
            Err(e) => stack.replace(ctx, (Value::Nil, e.to_string())),
        }
        Ok(CallbackReturn::Return)
    });
    handle.set(ctx, "recv", recv).ok();

    let held = Rc::clone(&socket);
    let close = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        // Taking the stream is the close, and makes a second close a no-op rather than an error.
        held.borrow_mut().take();
        stack.replace(ctx, true);
        Ok(CallbackReturn::Return)
    });
    handle.set(ctx, "close", close).ok();

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::os::unix::net::UnixListener;

    /// A scratch runtime with a family directory, a socket in it, and the session bus beside it.
    fn runtime() -> (Scratch, PathBuf, Vec<UnixListener>) {
        let dir = Scratch::new("stream", "family");
        let root = std::fs::canonicalize(&*dir).expect("canonical scratch");
        std::fs::create_dir(root.join("magi")).expect("family dir");
        let held = vec![
            UnixListener::bind(root.join("magi/host.sock")).expect("family socket"),
            UnixListener::bind(root.join("bus")).expect("bus socket"),
        ];
        (dir, root, held)
    }

    #[test]
    fn a_family_socket_is_dialable() {
        let (_dir, root, _held) = runtime();
        let roots = [root.join("magi")];
        assert_eq!(
            dialable_in(&root.join("magi/host.sock"), &roots),
            Some(root.join("magi/host.sock"))
        );
    }

    #[test]
    fn the_session_bus_beside_the_family_is_refused() {
        let (_dir, root, _held) = runtime();
        let roots = [root.join("magi")];
        assert_eq!(dialable_in(&root.join("bus"), &roots), None);
        assert_eq!(dialable_in(&root.join("magi/../bus"), &roots), None);
    }

    #[test]
    fn a_link_or_a_plain_file_inside_a_family_dir_is_refused() {
        let (_dir, root, _held) = runtime();
        let roots = [root.join("magi")];
        std::os::unix::fs::symlink(root.join("bus"), root.join("magi/link")).expect("link");
        std::os::unix::fs::symlink(&root, root.join("magi/up")).expect("dir link");
        std::fs::write(root.join("magi/file"), "x").expect("file");
        for name in ["magi/link", "magi/up/bus", "magi/file", "magi/absent"] {
            assert_eq!(dialable_in(&root.join(name), &roots), None, "{name}");
        }
    }

    #[test]
    fn a_relative_path_is_never_dialable() {
        assert_eq!(dialable(Path::new("bus")), None);
        assert_eq!(dialable(Path::new("")), None);
    }

    #[test]
    fn a_config_cannot_open_the_session_bus() {
        let (_dir, root, _held) = runtime();
        let mut engine = crate::lua::engine::Engine::new();
        let source = format!(
            "local h, why = casper.stream.connect({:?})\ncasper.answer = tostring(h) .. ' ' .. why",
            root.join("bus").display().to_string()
        );
        engine.run(&source, "probe.lua").expect("runs");
        engine.harvest();
        let said = engine.setting("answer").unwrap_or_default();
        assert!(
            said.as_str().is_some_and(|s| s.starts_with("nil ")),
            "{said}"
        );
    }
}
