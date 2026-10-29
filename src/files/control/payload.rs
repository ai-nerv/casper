use super::{WAIT, invalid, receive, send};
use rustix::fs::{self, FileType, MemfdFlags, SealFlags};
use sha2::{Digest, Sha256};
use std::io::{Result, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::FileExt;
use std::time::Instant;

const MAGIC: &[u8] = b"CFP1";
const MAX: usize = crate::framing::MOST;

fn seals() -> SealFlags {
    SealFlags::SEAL | SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK
}

pub fn send_request(channel: BorrowedFd<'_>, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX {
        return Err(invalid("native file request exceeds its bounds"));
    }
    let fd = fs::memfd_create(
        "casper-file-request",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes)?;
    fs::fcntl_add_seals(&file, seals())?;
    let mut body = MAGIC.to_vec();
    body.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    body.extend_from_slice(&Sha256::digest(bytes));
    send(channel, &body, &[file.as_fd()])
}

pub fn receive_request(channel: BorrowedFd<'_>) -> Result<Vec<u8>> {
    let (body, mut rights) = receive(channel, Instant::now() + WAIT)?;
    if body.len() != MAGIC.len() + 8 + 32 || !body.starts_with(MAGIC) || rights.len() != 1 {
        return Err(invalid("invalid native file request envelope"));
    }
    let encoded: [u8; 8] = body
        .get(4..12)
        .ok_or_else(|| invalid("missing request size"))?
        .try_into()
        .map_err(|_| invalid("invalid request size"))?;
    let size = usize::try_from(u64::from_le_bytes(encoded))
        .map_err(|_| invalid("request size overflow"))?;
    if size == 0 || size > MAX {
        return Err(invalid("native file request exceeds its bounds"));
    }
    let fd = rights
        .pop()
        .ok_or_else(|| invalid("request descriptor missing"))?;
    let metadata = fs::fstat(&fd)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_nlink != 0
        || metadata.st_uid != rustix::process::geteuid().as_raw()
        || metadata.st_size < 0
        || metadata.st_size as u64 != size as u64
        || !fs::fcntl_get_seals(&fd)?.contains(seals())
        || !rustix::io::fcntl_getfd(&fd)?.contains(rustix::io::FdFlags::CLOEXEC)
    {
        return Err(invalid(
            "native request descriptor is not sealed and complete",
        ));
    }
    let file = std::fs::File::from(fd);
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if body.get(12..) != Some(digest.as_slice()) {
        return Err(invalid("native request digest mismatch"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::super::{pair, send};
    use super::*;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn complete_request_payload_exceeds_presentation_and_argv_caps() {
        let bytes = vec![b'x'; 2 * 1024 * 1024 + 17];
        let (sender, receiver) = pair().expect("channel");
        send_request(sender.as_fd(), &bytes).expect("sealed request");
        drop(sender);
        assert_eq!(
            receive_request(receiver.as_fd()).expect("complete immutable payload"),
            bytes
        );
    }

    #[test]
    fn unsealed_wrong_size_and_digest_payloads_refuse_and_offsets_are_not_shared() {
        for fault in ["unsealed", "size", "digest", "offset"] {
            let (sender, receiver) = pair().expect("channel");
            let fd = fs::memfd_create(
                "request-fixture",
                MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
            )
            .expect("anonymous request");
            let mut file = std::fs::File::from(fd);
            let bytes = b"complete request";
            file.write_all(bytes).expect("payload");
            if fault != "unsealed" {
                fs::fcntl_add_seals(&file, seals()).expect("seals");
            }
            let mut body = MAGIC.to_vec();
            body.extend_from_slice(
                &((bytes.len() + usize::from(fault == "size")) as u64).to_le_bytes(),
            );
            body.extend_from_slice(&Sha256::digest(bytes));
            if fault == "digest" {
                *body.last_mut().expect("digest") ^= 1;
            }
            send(sender.as_fd(), &body, &[file.as_fd()]).expect("fault payload");
            file.seek(SeekFrom::Start(4)).expect("change shared offset");
            let received = receive_request(receiver.as_fd());
            if fault == "offset" {
                assert_eq!(received.expect("positional acquisition"), bytes);
                assert_eq!(file.stream_position().expect("shared cursor"), 4);
                assert!(file.write_all(b"changed").is_err(), "sealed bytes changed");
            } else {
                assert!(received.is_err(), "accepted {fault}");
            }
        }
    }

    #[test]
    fn oversized_and_empty_requests_do_not_enqueue_a_descriptor() {
        let (sender, receiver) = pair().expect("channel");
        assert!(send_request(sender.as_fd(), b"").is_err());
        assert!(send_request(sender.as_fd(), &vec![0; MAX + 1]).is_err());
        assert!(
            matches!(receive(receiver.as_fd(), Instant::now() + std::time::Duration::from_millis(30)),
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut)
        );
    }
}
