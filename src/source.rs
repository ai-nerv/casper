//! Complete bounded text snapshots for jailed edit acquisition.

use serde::{Deserialize, Serialize};
use std::io::Read;

pub const EDIT_BYTES: usize = crate::source_worker::EDIT_BYTES;
const REPLY_BYTES: usize = EDIT_BYTES * 6 + 4096;
pub const FLAG: &str = "--source-text";
pub const REPLACE: &str = "replace-source";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("editable text must be smaller than 1 MiB")]
    TooLarge,
    #[error("editable text must be valid UTF-8 without NUL bytes")]
    InvalidText,
    #[error("source IO failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("source worker failed: {0}")]
    Reader(String),
    #[error("source reader returned an incomplete or invalid snapshot")]
    Protocol,
}

#[derive(Serialize, Deserialize)]
pub struct Complete {
    source_text: String,
    source_bytes: usize,
}

#[derive(Serialize, Deserialize)]
pub struct Stored {
    written_bytes: usize,
}

fn within<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    within_by(crate::running::READING, operation)
}

fn within_by<T: Send + 'static>(
    deadline: std::time::Duration,
    operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("source-io".into())
        .spawn(move || {
            let _ = send.send(operation());
        })?;
    match receive.recv_timeout(deadline) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "source IO deadline expired",
        ))),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(Error::Reader("source IO worker stopped".into()))
        }
    }
}

/// Read one complete snapshot in the source worker's filesystem namespace.
pub fn read(path: &str) -> Result<Complete, Error> {
    if crate::jail::Jail::from_env().on() {
        let source_text = jailed(path)?;
        return Ok(Complete {
            source_bytes: source_text.len(),
            source_text,
        });
    }
    let path = path.to_owned();
    within(move || read_from(&mut std::fs::File::open(path)?))
}

/// Replace an existing source with bounded complete stdin in the current worker namespace.
pub fn store(path: &str) -> Result<Stored, Error> {
    let text = within(|| read_from(&mut std::io::stdin().lock()))?;
    if crate::jail::Jail::from_env().on() {
        replace(path, &text.source_text)?;
        return Ok(Stored {
            written_bytes: text.source_bytes,
        });
    }
    let path = path.to_owned();
    within(move || {
        let written_bytes = crate::source_worker::replace(&path, &text.source_text)?;
        Ok(Stored { written_bytes })
    })
}

fn operand(path: &str) -> String {
    if std::path::Path::new(path).is_relative() {
        format!("./{path}")
    } else {
        path.to_owned()
    }
}

/// Send bounded replacement text to this executable inside the current jail.
pub fn replace(path: &str, text: &str) -> Result<(), Error> {
    if text.len() >= EDIT_BYTES {
        return Err(Error::TooLarge);
    }
    if text.as_bytes().contains(&0) {
        return Err(Error::InvalidText);
    }
    let executable = std::env::current_exe()?;
    let executable = executable.to_str().ok_or(Error::Protocol)?;
    let done = crate::running::fed(
        executable,
        &[REPLACE.into(), operand(path)],
        Some(text.as_bytes().to_vec()),
    );
    if done.code != 0 {
        return Err(Error::Reader(done.err));
    }
    decoded_write(done.out.as_bytes(), text.len())
}

fn decoded_write(bytes: &[u8], expected: usize) -> Result<(), Error> {
    let reply = answer(bytes)?;
    if reply.n != 1 || reply.result.len() != 1 {
        return Err(Error::Protocol);
    }
    let value = reply.result.into_iter().next().ok_or(Error::Protocol)?;
    let stored: Stored = serde_json::from_value(value).map_err(|_| Error::Protocol)?;
    if stored.written_bytes != expected {
        return Err(Error::Protocol);
    }
    Ok(())
}

fn read_from(reader: &mut impl Read) -> Result<Complete, Error> {
    let mut bytes = Vec::new();
    reader.take(EDIT_BYTES as u64).read_to_end(&mut bytes)?;
    if bytes.len() == EDIT_BYTES {
        return Err(Error::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(Error::InvalidText);
    }
    let source_bytes = bytes.len();
    let source_text = String::from_utf8(bytes).map_err(|_| Error::InvalidText)?;
    Ok(Complete {
        source_text,
        source_bytes,
    })
}

/// Acquire a complete text snapshot through this executable inside the current jail.
pub fn jailed(path: &str) -> Result<String, Error> {
    let executable = std::env::current_exe()?;
    let executable = executable.to_str().ok_or(Error::Protocol)?;
    let path = operand(path);
    let bytes =
        crate::running::binary(executable, &["code".into(), FLAG.into(), path], REPLY_BYTES)
            .map_err(Error::Reader)?;
    decoded(&bytes)
}

fn answer(bytes: &[u8]) -> Result<crate::wire::Reply, Error> {
    let reply: crate::wire::Reply = serde_json::from_slice(bytes).map_err(|_| Error::Protocol)?;
    if reply.family != crate::wire::FAMILY {
        return Err(Error::Protocol);
    }
    if !reply.ok {
        return Err(Error::Reader(
            reply.error.unwrap_or_else(|| "acquisition refused".into()),
        ));
    }
    Ok(reply)
}

fn decoded(bytes: &[u8]) -> Result<String, Error> {
    let reply = answer(bytes)?;
    if reply.n != 1 || reply.result.len() != 1 {
        return Err(Error::Protocol);
    }
    let value = reply.result.into_iter().next().ok_or(Error::Protocol)?;
    let complete: Complete = serde_json::from_value(value).map_err(|_| Error::Protocol)?;
    if complete.source_bytes != complete.source_text.len()
        || complete.source_bytes >= EDIT_BYTES
        || complete.source_text.as_bytes().contains(&0)
    {
        return Err(Error::Protocol);
    }
    Ok(complete.source_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_complete_at_the_byte_limit_and_strict_about_encoding() {
        let text = vec![b'x'; EDIT_BYTES - 1];
        assert_eq!(
            read_from(&mut text.as_slice())
                .expect("bounded text")
                .source_text
                .as_bytes(),
            text
        );
        assert!(matches!(
            read_from(&mut vec![b'x'; EDIT_BYTES].as_slice()),
            Err(Error::TooLarge)
        ));
        for bytes in [&b"\xff"[..], &b"valid\0text"[..]] {
            assert!(matches!(read_from(&mut &*bytes), Err(Error::InvalidText)));
        }
    }

    #[test]
    fn an_error_after_a_prefix_never_returns_a_snapshot() {
        struct Broken(bool);
        impl Read for Broken {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("synthetic read failure"));
                }
                self.0 = true;
                let mut prefix = &b"old\n"[..];
                prefix.read(bytes)
            }
        }
        assert!(matches!(read_from(&mut Broken(false)), Err(Error::Io(_))));
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let (finished, ended) = std::sync::mpsc::sync_channel(1);
        let result = within_by(std::time::Duration::ZERO, move || {
            released
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("release IO worker");
            finished.send(()).expect("IO worker ended");
            Ok(())
        });
        assert!(
            matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        release.send(()).expect("release");
        ended
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("bounded worker teardown");
    }

    #[test]
    fn every_truncated_reply_and_inconsistent_snapshot_is_refused() {
        let source = read_from(&mut "prefix\"} €\r\ntail".as_bytes()).expect("source");
        let reply = crate::wire::Reply::of(serde_json::to_value(source).expect("snapshot"));
        let bytes = serde_json::to_vec(&reply).expect("reply");
        assert_eq!(
            decoded(&bytes).expect("complete reply"),
            "prefix\"} €\r\ntail"
        );
        for end in 0..bytes.len() {
            assert!(
                decoded(&bytes[..end]).is_err(),
                "accepted byte prefix {end}"
            );
        }
        for value in [
            serde_json::json!({"source_text":"prefix","source_bytes":100}),
            serde_json::json!({"source_text":"prefix"}),
            serde_json::json!({"source_text":"prefix\0","source_bytes":7}),
            serde_json::json!({"path":"source.txt","language":"rust"}),
        ] {
            let reply = crate::wire::Reply::of(value);
            assert!(matches!(
                decoded(&serde_json::to_vec(&reply).expect("reply")),
                Err(Error::Protocol)
            ));
        }
        let mut wrong_family = reply.clone();
        wrong_family.family += 1;
        assert!(decoded(&serde_json::to_vec(&wrong_family).expect("reply")).is_err());
        let stored = crate::wire::Reply::of(serde_json::json!({"written_bytes":7}));
        let bytes = serde_json::to_vec(&stored).expect("write reply");
        assert!(decoded_write(&bytes, 7).is_ok());
        assert!(decoded_write(&bytes, 8).is_err());
        for end in 0..bytes.len() {
            assert!(decoded_write(&bytes[..end], 7).is_err());
        }
    }
}
