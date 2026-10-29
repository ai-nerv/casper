use rustix::fs::{self, FileType, FlockOperation, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{Error, ErrorKind, Result};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MAX_ENTRIES: usize = 4096;
pub const MAX_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitFor {
    Registry,
    Target,
}

/// Parent object and leaf identity, independent of the target inode.
pub struct Binding {
    _parent: OwnedFd,
    key: Key,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Key(String);

impl Key {
    pub fn from_wire(value: &str) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("unsupported file binding key"));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Binding {
    pub fn from_parent(parent: OwnedFd, leaf: &OsStr) -> Result<Self> {
        let bytes = leaf.as_encoded_bytes();
        if bytes.is_empty()
            || bytes.len() > 255
            || matches!(bytes, b"." | b"..")
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(invalid("file binding needs one bounded leaf name"));
        }
        let metadata = live_parent(&parent)?;
        let mut digest = Sha256::new();
        digest.update(b"casper:file-binding:1\0");
        digest.update(metadata.st_dev.to_le_bytes());
        digest.update(metadata.st_ino.to_le_bytes());
        digest.update(bytes);
        Ok(Self {
            _parent: parent,
            key: Key(format!("{:x}", digest.finalize())),
        })
    }

    pub fn key(&self) -> &str {
        self.key.as_str()
    }

    pub fn identity(&self) -> &Key {
        &self.key
    }

    pub(super) fn validate(&self) -> Result<()> {
        live_parent(&self._parent).map(|_| ())
    }
}

/// An already opened private registry; no environment-selected namespace.
pub struct Registry {
    directory: OwnedFd,
}

/// Retains the target lock, registry and parent binding until dropped.
#[must_use = "retain the lease through the file operation"]
pub struct Lease<'a> {
    _file: OwnedFd,
    _registry: &'a Registry,
    _binding: Option<&'a Binding>,
    key: Key,
}

impl Lease<'_> {
    pub fn handoff(&self, channel: std::os::fd::BorrowedFd<'_>) -> Result<()> {
        self._registry.checked_lease(&self.key, &self._file)?;
        if let Some(binding) = self._binding {
            binding.validate()?;
        }
        super::control::send_lease(channel, &self.key, &self._file)
    }
}

impl Registry {
    pub fn from_private(directory: OwnedFd) -> Result<Self> {
        private(&directory, FileType::Directory, 0o700)?;
        let directory = fs::openat(
            &directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self { directory })
    }

    pub fn lease<'a>(
        &'a self,
        binding: &'a Binding,
        wait: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Lease<'a>> {
        self.lease_with(binding, wait, cancelled, |_| {})
    }

    fn lease_with<'a>(
        &'a self,
        binding: &'a Binding,
        wait: Duration,
        cancelled: &AtomicBool,
        waiting: impl FnMut(WaitFor),
    ) -> Result<Lease<'a>> {
        let mut lease = self.lease_key(&binding.key, wait, cancelled, waiting)?;
        binding.validate()?;
        lease._binding = Some(binding);
        Ok(lease)
    }

    pub fn delegate<'a>(
        &'a self,
        key: &Key,
        wait: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Lease<'a>> {
        self.lease_key(key, wait, cancelled, |_| {})
    }

    fn lease_key<'a>(
        &'a self,
        key: &Key,
        wait: Duration,
        cancelled: &AtomicBool,
        mut waiting: impl FnMut(WaitFor),
    ) -> Result<Lease<'a>> {
        if wait > MAX_WAIT {
            return Err(invalid("file lease wait exceeds five seconds"));
        }
        let deadline = Instant::now() + wait;
        check_cancelled(cancelled)?;
        private(&self.directory, FileType::Directory, 0o700)?;
        let admission = fs::openat(
            &self.directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        lock(&admission, deadline, cancelled, &mut || {
            waiting(WaitFor::Registry)
        })
        .map_err(|error| stage("registry admission", error))?;
        private(&self.directory, FileType::Directory, 0o700)?;
        let file = self.reserved(key.as_str())?;
        drop(admission);
        lock(&file, deadline, cancelled, &mut || waiting(WaitFor::Target))
            .map_err(|error| stage("target acquisition", error))?;
        self.checked_lease(key, &file)?;
        check_cancelled(cancelled)?;
        Ok(Lease {
            _file: file,
            _registry: self,
            _binding: None,
            key: key.clone(),
        })
    }

    fn checked_lease(&self, key: &Key, file: &OwnedFd) -> Result<()> {
        private(&self.directory, FileType::Directory, 0o700)?;
        private(file, FileType::RegularFile, 0o600)?;
        let named = fs::statat(&self.directory, key.as_str(), fs::AtFlags::SYMLINK_NOFOLLOW)?;
        let opened = fs::fstat(file)?;
        if (named.st_dev, named.st_ino) != (opened.st_dev, opened.st_ino) {
            return Err(invalid("file lease registry binding changed"));
        }
        Ok(())
    }

    fn reserved(&self, key: &str) -> Result<OwnedFd> {
        match fs::openat(
            &self.directory,
            key,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(pinned) => {
                private(&pinned, FileType::RegularFile, 0o600)?;
                let file = fs::open(
                    format!("/proc/self/fd/{}", pinned.as_raw_fd()),
                    OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                let expected = fs::fstat(&pinned)?;
                let actual = fs::fstat(&file)?;
                if (expected.st_dev, expected.st_ino) != (actual.st_dev, actual.st_ino) {
                    return Err(invalid("file lease descriptor binding changed"));
                }
                Ok(file)
            }
            Err(rustix::io::Errno::NOENT) => {
                self.capacity()?;
                let file = fs::openat(
                    &self.directory,
                    key,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )?;
                fs::fchmod(&file, Mode::RUSR | Mode::WUSR)?;
                private(&file, FileType::RegularFile, 0o600)?;
                Ok(file)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn capacity(&self) -> Result<()> {
        let entries = fs::Dir::read_from(&self.directory)?;
        let mut count = 0;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if name.len() != 64 || !name.iter().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid("unexpected entry in file lease registry"));
            }
            count += 1;
            if count >= MAX_ENTRIES {
                return Err(Error::other("file lease registry capacity reached"));
            }
        }
        Ok(())
    }
}

fn private(fd: &OwnedFd, kind: FileType, mode: u32) -> Result<()> {
    let metadata = fs::fstat(fd)?;
    if FileType::from_raw_mode(metadata.st_mode) != kind
        || metadata.st_uid != rustix::process::geteuid().as_raw()
        || metadata.st_mode & 0o7777 != mode
        || metadata.st_nlink == 0
        || (kind == FileType::RegularFile && (metadata.st_nlink != 1 || metadata.st_size != 0))
    {
        return Err(invalid(
            "file lease registry object is not private and regular",
        ));
    }
    cloexec(fd)
}

pub(super) fn received(fd: &OwnedFd) -> Result<()> {
    private(fd, FileType::RegularFile, 0o600)
}

fn cloexec(fd: &OwnedFd) -> Result<()> {
    if !rustix::io::fcntl_getfd(fd)?.contains(rustix::io::FdFlags::CLOEXEC) {
        return Err(invalid("file lease descriptors must be close-on-exec"));
    }
    Ok(())
}

fn live_parent(fd: &OwnedFd) -> Result<fs::Stat> {
    let metadata = fs::fstat(fd)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory || metadata.st_nlink == 0 {
        return Err(invalid("file binding parent is not a live directory"));
    }
    cloexec(fd)?;
    Ok(metadata)
}

fn lock(
    fd: &OwnedFd,
    deadline: Instant,
    cancelled: &AtomicBool,
    waiting: &mut impl FnMut(),
) -> Result<()> {
    lock_using(deadline, cancelled, waiting, || {
        fs::flock(fd, FlockOperation::NonBlockingLockExclusive)
    })
}

fn lock_using(
    deadline: Instant,
    cancelled: &AtomicBool,
    waiting: &mut impl FnMut(),
    mut attempt: impl FnMut() -> rustix::io::Result<()>,
) -> Result<()> {
    loop {
        check_cancelled(cancelled)?;
        match attempt() {
            Ok(()) => return check_cancelled(cancelled),
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::INTR =>
            {
                if error == rustix::io::Errno::WOULDBLOCK {
                    waiting();
                }
                check_cancelled(cancelled)?;
                let now = Instant::now();
                if now >= deadline {
                    return Err(Error::new(ErrorKind::TimedOut, "file lease wait expired"));
                }
                std::thread::sleep(POLL.min(deadline - now));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(Error::new(ErrorKind::Interrupted, "file lease cancelled"))
    } else {
        Ok(())
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

fn stage(name: &str, error: Error) -> Error {
    Error::new(error.kind(), format!("file lease {name}: {error}"))
}

#[cfg(test)]
mod tests;
