use super::protocol::{Payload, REPLY_MOST, Request, Response};
use super::{Catalog, Failure};
use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub fn invoke(request: &Request, cancel: &AtomicBool) -> Result<Payload, Failure> {
    request.validate()?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Failure::Cancelled);
    }
    let prepared = match crate::jail::workflow(&request.root) {
        Ok(prepared) => prepared,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && matches!(request.operation, super::protocol::Operation::Catalog {}) =>
        {
            if matches!(
                super::Root::open(&request.root, &super::Budget::new()),
                Err(Failure::NotFound)
            ) {
                return Ok(Payload::Catalog(Catalog {
                    entries: Vec::new(),
                    invalid_names: 0,
                }));
            }
            return Err(Failure::Worker);
        }
        Err(_) => return Err(Failure::Worker),
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(Failure::Cancelled);
    }
    let input = serde_json::to_vec(request).map_err(|_| Failure::Protocol)?;
    let bytes = exchange(prepared, &input, cancel, Duration::from_secs(5))?;
    let response: Response = serde_json::from_slice(&bytes).map_err(|_| Failure::Protocol)?;
    response.validate(request)?;
    response.result
}

struct Owned {
    child: Child,
}

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(
            rustix::process::Pid::from_child(&self.child),
            rustix::process::Signal::KILL,
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), Failure> {
    let flags = rustix::fs::fcntl_getfl(fd).map_err(|_| Failure::Worker)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK).map_err(|_| Failure::Worker)
}

fn exchange(
    mut prepared: crate::jail::Prepared,
    input: &[u8],
    cancel: &AtomicBool,
    limit: Duration,
) -> Result<Vec<u8>, Failure> {
    if input.len() > super::protocol::INPUT_MOST {
        return Err(Failure::Protocol);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Failure::Cancelled);
    }
    if limit.is_zero() {
        return Err(Failure::Timeout);
    }
    let deadline = Instant::now() + limit;
    let mut owned = Owned {
        child: prepared
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| Failure::Worker)?,
    };
    let mut stdin = owned.child.stdin.take();
    let mut stdout = owned.child.stdout.take();
    let mut stderr = owned.child.stderr.take();
    nonblocking(stdin.as_ref().ok_or(Failure::Worker)?)?;
    nonblocking(stdout.as_ref().ok_or(Failure::Worker)?)?;
    nonblocking(stderr.as_ref().ok_or(Failure::Worker)?)?;
    let mut sent = 0;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut status = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Failure::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Timeout);
        }
        if let Some(pipe) = &mut stdin {
            match pipe.write(input.get(sent..).ok_or(Failure::Protocol)?) {
                Ok(0) if sent < input.len() => return Err(Failure::Worker),
                Ok(written) => sent += written,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(Failure::Worker),
            }
            if sent == input.len() {
                stdin = None;
            }
        }
        read_pipe(&mut stdout, &mut output, REPLY_MOST)?;
        read_pipe(&mut stderr, &mut errors, 4096)?;
        if status.is_none() {
            use rustix::process::{WaitId, WaitIdOptions, waitid};
            status = waitid(
                WaitId::Pid(rustix::process::Pid::from_child(&owned.child)),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|_| Failure::Worker)?;
        }
        if let Some(status) = &status
            && stdout.is_none()
            && stderr.is_none()
        {
            if status.exit_status() != Some(0) || sent != input.len() {
                return Err(Failure::Worker);
            }
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests;

fn read_pipe<T: Read>(
    pipe: &mut Option<T>,
    output: &mut Vec<u8>,
    most: usize,
) -> Result<(), Failure> {
    let Some(reader) = pipe else {
        return Ok(());
    };
    let mut buffer = [0_u8; 8192];
    match reader.read(&mut buffer) {
        Ok(0) => *pipe = None,
        Ok(count) => {
            if count > most.saturating_sub(output.len()) {
                return Err(Failure::Protocol);
            }
            output.extend_from_slice(buffer.get(..count).ok_or(Failure::Protocol)?);
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) => {}
        Err(_) => return Err(Failure::Worker),
    }
    Ok(())
}
