use super::{control, registry::Domain};
use std::io::{Error, ErrorKind, Read, Result};
use std::os::fd::AsFd;
use std::process::{Child, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

mod presence;
mod protocol;
mod reading;
mod snapshot;
mod staging;
mod transaction;
pub use presence::{Presence, State as PresenceState};
pub use protocol::{Guard, Operation, Request};
pub use reading::Reading;
pub use snapshot::Snapshot;
pub use transaction::{Commit, Failure};

pub const FLAG: &str = "--file-worker";
pub const OBSERVE: &str = "--file-observe";
pub const TRANSACT: &str = "--file-transaction";
const REPLY_BYTES: usize = (crate::source::EDIT_BYTES + crate::running::MOST) * 6 + 8192;

pub fn serve() -> Result<serde_json::Value> {
    if std::env::var("CASPER_NATIVE_FILE_WORKER").ok().as_deref() != Some("1") {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "native file worker needs a prepared channel",
        ));
    }
    let input = std::io::stdin();
    let request: Request = serde_json::from_slice(
        &control::receive_request(input.as_fd()).map_err(|e| stage("request receive", e))?,
    )
    .map_err(Error::other)?;
    request.validate()?;
    let resolved = if matches!(
        request.operation,
        Operation::Presence | Operation::Read { .. }
    ) {
        snapshot::Resolved::bind(&request.path)
    } else if matches!(request.operation, Operation::Create { .. }) {
        snapshot::Resolved::resolve(&request.path, true)
    } else {
        snapshot::Resolved::open(&request.path)
    }
    .map_err(|e| stage("source binding", e))?;
    control::send_binding(input.as_fd(), &resolved.binding)
        .map_err(|e| stage("binding send", e))?;
    let lease = control::receive_lease(input.as_fd(), &resolved.binding)
        .map_err(|e| stage("lease receive", e))?;
    if let Operation::Read { offset, limit } = request.operation {
        serde_json::to_value(
            resolved
                .read(offset, limit)
                .map_err(|e| stage("read acquisition", e))?,
        )
    } else if matches!(request.operation, Operation::Presence) {
        serde_json::to_value(
            resolved
                .presence()
                .map_err(|e| stage("presence acquisition", e))?,
        )
    } else if matches!(request.operation, Operation::Observe) {
        serde_json::to_value(
            resolved
                .capture()
                .map_err(|e| stage("snapshot acquisition", e))?,
        )
    } else {
        serde_json::to_value(
            transaction::run(&resolved, &lease, &request).map_err(|e| stage("transaction", e))?,
        )
    }
    .map_err(Error::other)
}

struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn observe(path: &str) -> Result<Snapshot> {
    let request = Request {
        path: path.to_owned(),
        operation: Operation::Observe,
        guard: None,
    };
    let (key, value) = exchange(&request)?;
    let snapshot: Snapshot = serde_json::from_value(value).map_err(Error::other)?;
    snapshot.validate(&key)?;
    Ok(snapshot)
}

pub fn presence(path: &str) -> Result<Presence> {
    let request = Request {
        path: path.to_owned(),
        operation: Operation::Presence,
        guard: None,
    };
    let (key, value) = exchange(&request)?;
    let presence: Presence = serde_json::from_value(value).map_err(Error::other)?;
    presence.validate(&key)?;
    Ok(presence)
}

pub fn read(path: &str, offset: usize, limit: usize) -> Result<Reading> {
    let request = Request {
        path: path.to_owned(),
        operation: Operation::Read { offset, limit },
        guard: None,
    };
    let (key, value) = exchange(&request)?;
    let reading: Reading = serde_json::from_value(value).map_err(Error::other)?;
    reading.validate(&key, offset, limit)?;
    Ok(reading)
}

pub fn mutation(request: &Request) -> std::result::Result<Commit, Failure> {
    request.validate().map_err(Failure::Refused)?;
    if matches!(
        request.operation,
        Operation::Observe | Operation::Presence | Operation::Read { .. }
    ) {
        return Err(Failure::Refused(Error::other(
            "transaction needs a mutation operation",
        )));
    }
    let (key, value) = exchange(request).map_err(|error| {
        if error.get_ref().is_some_and(|inner| inner.is::<Rejected>()) {
            Failure::Refused(error)
        } else {
            Failure::Unknown(error)
        }
    })?;
    let commit: Commit =
        serde_json::from_value(value).map_err(|error| Failure::Unknown(Error::other(error)))?;
    commit.validate(&key, request).map_err(Failure::Unknown)?;
    Ok(commit)
}

pub fn mutation_input(input: impl Read) -> std::result::Result<Commit, Failure> {
    let mut bytes = Vec::new();
    input
        .take((crate::framing::MOST + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(Failure::Refused)?;
    if bytes.len() > crate::framing::MOST {
        return Err(Failure::Refused(Error::other(
            "native request exceeds its bound",
        )));
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|e| Failure::Refused(Error::other(e)))?;
    mutation(&request)
}

fn exchange(request: &Request) -> Result<(super::locking::Key, serde_json::Value)> {
    request.validate()?;
    let bytes = serde_json::to_vec(&request).map_err(Error::other)?;
    let mut prepared = crate::jail::native_file_worker()
        .map_err(|e| rejected(stage("containment preparation", e)))?;
    let (coordinator, worker) = control::pair()?;
    let mut child = Owned(
        prepared
            .command()
            .stdin(Stdio::from(worker))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| rejected(stage("worker spawn", e)))?,
    );
    let output = child
        .0
        .stdout
        .take()
        .ok_or_else(|| Error::other("native worker stdout missing"))?;
    let errors = child
        .0
        .stderr
        .take()
        .ok_or_else(|| Error::other("native worker stderr missing"))?;
    let error_reader = std::thread::Builder::new()
        .name("native-file-errors".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            errors
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("native-file-reply".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let reply = output
                .take((REPLY_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .and_then(|bytes| {
                    if bytes.len() > REPLY_BYTES {
                        Err(Error::other("native file reply exceeded its bound"))
                    } else {
                        Ok(bytes)
                    }
                });
            let _ = send.send(reply);
        })?;
    let deadline = Instant::now() + crate::running::READING;
    let mut got_key = false;
    let exchange = (|| {
        control::send_request(coordinator.as_fd(), &bytes)?;
        let key = control::receive_binding(coordinator.as_fd())?;
        got_key = true;
        let domain = Domain::system().map_err(|e| stage("registry provisioning", e))?;
        let lease = domain
            .delegate(&key, super::locking::MAX_WAIT, &AtomicBool::new(false))
            .map_err(|e| stage("lease acquisition", e))?;
        domain
            .validate()
            .map_err(|e| stage("registry validation", e))?;
        lease
            .handoff(coordinator.as_fd())
            .map_err(|e| stage("lease handoff", e))?;
        Ok::<_, Error>(key)
    })();
    if exchange.is_err() {
        let _ = rustix::net::shutdown(coordinator.as_fd(), rustix::net::Shutdown::Both);
    }
    let bytes = loop {
        match receive.try_recv() {
            Ok(reply) => break reply?,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(Error::other("native reply reader stopped"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                ErrorKind::TimedOut,
                "native file worker deadline expired",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                ErrorKind::TimedOut,
                "native file worker did not exit",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    reader
        .join()
        .map_err(|_| Error::other("native reply reader panicked"))?;
    let errors = error_reader
        .join()
        .map_err(|_| Error::other("native error reader panicked"))??;
    if !prepared.restored().is_empty() {
        return Err(Error::other("native worker changed protected state"));
    }
    if !status.success() {
        return Err(Error::other(format!(
            "native file worker exited {status}: {}",
            String::from_utf8_lossy(&errors)
        )));
    }
    let reply = verified_reply(&bytes)?;
    if !reply.ok {
        return Err(rejected(match exchange {
            Err(error) if got_key => error,
            _ => Error::other(
                reply
                    .error
                    .unwrap_or_else(|| "native worker refused".into()),
            ),
        }));
    }
    let key = exchange?;
    let value = reply
        .result
        .into_iter()
        .next()
        .ok_or_else(|| Error::other("native snapshot missing"))?;
    Ok((key, value))
}

fn stage(name: &str, error: Error) -> Error {
    Error::new(error.kind(), format!("native {name}: {error}"))
}

#[derive(Debug)]
struct Rejected(String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

fn rejected(error: Error) -> Error {
    Error::new(error.kind(), Rejected(error.to_string()))
}

fn verified_reply(bytes: &[u8]) -> Result<crate::wire::Reply> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(Error::other)?;
    if ["ok", "family", "n", "result"]
        .iter()
        .any(|field| value.get(field).is_none())
    {
        return Err(Error::other("native worker reply is incomplete"));
    }
    let reply: crate::wire::Reply = serde_json::from_value(value).map_err(Error::other)?;
    if reply.family != crate::wire::FAMILY
        || reply.n != reply.result.len()
        || if reply.ok {
            reply.n != 1 || reply.error.is_some() || reply.fault.is_some()
        } else {
            reply.n != 0
                || reply.fault != Some(crate::wire::Fault::Refused)
                || reply
                    .error
                    .as_ref()
                    .is_none_or(|error| error.is_empty() || error.len() > 4096)
        }
    {
        return Err(Error::other(
            "native worker returned an incomplete or unsupported reply",
        ));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
pub(crate) use lifecycle_tests::probe as lifecycle_probe;
#[cfg(test)]
pub(crate) use lifecycle_tests::stage_closure_probe;
