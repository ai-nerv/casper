use super::locking::{Binding, Key};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::net::{self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags};
use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
use std::io::{Error, ErrorKind, IoSlice, IoSliceMut, Result};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

const BINDING: &[u8] = b"CFB1";
const LEASE: &[u8] = b"CFL1";
const KEY_BYTES: usize = 64;
const MAX_PACKET: usize = 128;
pub const WAIT: Duration = Duration::from_secs(5);

mod payload;
pub use payload::{receive_request, send_request};

/// A transferred lock held for the lifetime of its worker's pinned binding.
#[must_use = "retain the received lease through the file operation"]
pub struct ReceivedLease<'a> {
    _file: OwnedFd,
    _binding: &'a Binding,
}

pub fn pair() -> Result<(OwnedFd, OwnedFd)> {
    Ok(net::socketpair(
        net::AddressFamily::UNIX,
        net::SocketType::SEQPACKET,
        net::SocketFlags::CLOEXEC,
        None,
    )?)
}

#[cfg(test)]
pub(crate) fn send_lifecycle_probe(
    channel: BorrowedFd<'_>,
    marker: u8,
    pid: &OwnedFd,
) -> Result<()> {
    send(channel, &[b'C', b'F', b'T', b'1', marker], &[pid.as_fd()])
}

#[cfg(test)]
pub(crate) fn receive_lifecycle_probe(channel: BorrowedFd<'_>) -> Result<(u8, OwnedFd)> {
    let (body, mut rights) = receive(channel, Instant::now() + WAIT)?;
    if body.len() != 5 || !body.starts_with(b"CFT1") || rights.len() != 1 {
        return Err(invalid("invalid lifecycle probe"));
    }
    Ok((
        *body
            .get(4)
            .ok_or_else(|| invalid("missing lifecycle marker"))?,
        rights
            .pop()
            .ok_or_else(|| invalid("missing lifecycle pidfd"))?,
    ))
}

pub fn send_binding(channel: BorrowedFd<'_>, binding: &Binding) -> Result<()> {
    binding.validate()?;
    let mut body = BINDING.to_vec();
    body.extend_from_slice(binding.key().as_bytes());
    send(channel, &body, &[])
}

pub fn receive_binding(channel: BorrowedFd<'_>) -> Result<Key> {
    let (body, rights) = receive(channel, Instant::now() + WAIT)?;
    if !rights.is_empty() || body.len() != BINDING.len() + KEY_BYTES || !body.starts_with(BINDING) {
        return Err(invalid("invalid file binding message"));
    }
    key(&body, BINDING.len())
}

pub(super) fn send_lease(channel: BorrowedFd<'_>, key: &Key, fd: &OwnedFd) -> Result<()> {
    super::locking::received(fd)?;
    let metadata = rustix::fs::fstat(fd)?;
    let mut body = LEASE.to_vec();
    body.extend_from_slice(key.as_str().as_bytes());
    body.extend_from_slice(&metadata.st_dev.to_le_bytes());
    body.extend_from_slice(&metadata.st_ino.to_le_bytes());
    send(channel, &body, &[fd.as_fd()])
}

pub fn receive_lease<'a>(
    channel: BorrowedFd<'_>,
    binding: &'a Binding,
) -> Result<ReceivedLease<'a>> {
    let (body, mut rights) = receive(channel, Instant::now() + WAIT)?;
    if rights.len() != 1 || body.len() != LEASE.len() + KEY_BYTES + 16 || !body.starts_with(LEASE) {
        return Err(invalid("invalid file lease message"));
    }
    if key(&body, LEASE.len())? != *binding.identity() {
        return Err(invalid("file lease belongs to another binding"));
    }
    let fd = rights
        .pop()
        .ok_or_else(|| invalid("file lease descriptor missing"))?;
    super::locking::received(&fd)?;
    binding.validate()?;
    let metadata = rustix::fs::fstat(&fd)?;
    let mut identity = Vec::with_capacity(16);
    identity.extend_from_slice(&metadata.st_dev.to_le_bytes());
    identity.extend_from_slice(&metadata.st_ino.to_le_bytes());
    if body.get(LEASE.len() + KEY_BYTES..) != Some(identity.as_slice()) {
        return Err(invalid("file lease descriptor identity changed"));
    }
    Ok(ReceivedLease {
        _file: fd,
        _binding: binding,
    })
}

fn key(body: &[u8], offset: usize) -> Result<Key> {
    let bytes = body
        .get(offset..offset + KEY_BYTES)
        .ok_or_else(|| invalid("short file key"))?;
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid file key encoding"))?;
    Key::from_wire(text)
}

fn channel(fd: BorrowedFd<'_>) -> Result<()> {
    if net::sockopt::socket_type(fd)? != net::SocketType::SEQPACKET
        || net::sockopt::socket_domain(fd)? != net::AddressFamily::UNIX
    {
        return Err(invalid("file control needs a private Unix packet socket"));
    }
    let flags = rustix::io::fcntl_getfd(fd)?;
    rustix::io::fcntl_setfd(fd, flags | rustix::io::FdFlags::CLOEXEC)?;
    Ok(())
}

fn send(fd: BorrowedFd<'_>, body: &[u8], rights: &[BorrowedFd<'_>]) -> Result<()> {
    channel(fd)?;
    if body.is_empty() || body.len() > MAX_PACKET || rights.len() > 1 {
        return Err(invalid("file control packet exceeds its bounds"));
    }
    let deadline = Instant::now() + WAIT;
    loop {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        if !rights.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(rights)) {
            return Err(invalid("file control descriptor cannot be encoded"));
        }
        match net::sendmsg(
            fd,
            &[IoSlice::new(body)],
            &mut ancillary,
            SendFlags::NOSIGNAL | SendFlags::DONTWAIT,
        ) {
            Ok(written) if written == body.len() => return Ok(()),
            Ok(_) => return Err(invalid("file control packet was incomplete")),
            Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => {
                ready(fd, PollFlags::OUT, deadline)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn receive(fd: BorrowedFd<'_>, deadline: Instant) -> Result<(Vec<u8>, Vec<OwnedFd>)> {
    channel(fd)?;
    loop {
        let mut body = [0; MAX_PACKET];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let message = net::recvmsg(
            fd,
            &mut [IoSliceMut::new(&mut body)],
            &mut ancillary,
            RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
        );
        match message {
            Ok(message) => {
                if message
                    .flags
                    .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
                {
                    return Err(invalid("truncated file control packet"));
                }
                let mut rights = Vec::new();
                for message in ancillary.drain() {
                    match message {
                        RecvAncillaryMessage::ScmRights(fds) => rights.extend(fds),
                        _ => return Err(invalid("unexpected file control ancillary message")),
                    }
                }
                if message.bytes == 0 {
                    return Err(Error::new(ErrorKind::UnexpectedEof, "file control closed"));
                }
                let body = body
                    .get(..message.bytes)
                    .ok_or_else(|| invalid("invalid file control length"))?;
                return Ok((body.to_vec(), rights));
            }
            Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => {
                ready(fd, PollFlags::IN, deadline)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn ready(fd: BorrowedFd<'_>, interest: PollFlags, deadline: Instant) -> Result<()> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::new(
                ErrorKind::TimedOut,
                "file control deadline expired",
            ));
        }
        let timeout = Timespec::try_from(deadline - now).map_err(Error::other)?;
        match rustix::event::poll(&mut [PollFd::new(&fd, interest)], Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
