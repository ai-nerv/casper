use super::locking::{Key, Lease, Registry};
use rustix::fs::{self, FileType, Mode, OFlags};
use std::io::{Error, ErrorKind, Result};
use std::os::fd::OwnedFd;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub struct Domain {
    anchor: OwnedFd,
    directory: OwnedFd,
    registry: Registry,
    leaf: std::ffi::OsString,
}

impl Domain {
    pub fn system() -> Result<Self> {
        let root = crate::jail::file_registry_path();
        let leaf = root
            .file_name()
            .ok_or_else(|| Error::other("invalid fixed registry path"))?
            .to_owned();
        let anchor = fs::open(
            "/var/tmp",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        validate_anchor(&anchor)?;
        match fs::mkdirat(&anchor, &leaf, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let directory = fs::openat(
            &anchor,
            &leaf,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let registry = Registry::from_private(directory.try_clone()?)?;
        let domain = Self {
            anchor,
            directory,
            registry,
            leaf,
        };
        domain.validate()?;
        Ok(domain)
    }

    pub fn validate(&self) -> Result<()> {
        validate_anchor(&self.anchor)?;
        let opened = fs::fstat(&self.directory)?;
        let named = fs::statat(&self.anchor, &self.leaf, fs::AtFlags::SYMLINK_NOFOLLOW)?;
        if (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino)
            || FileType::from_raw_mode(opened.st_mode) != FileType::Directory
            || opened.st_uid != rustix::process::geteuid().as_raw()
            || opened.st_mode & 0o7777 != 0o700
            || opened.st_nlink == 0
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "fixed file registry binding is unsafe",
            ));
        }
        Ok(())
    }

    pub fn delegate(&self, key: &Key, wait: Duration, cancelled: &AtomicBool) -> Result<Lease<'_>> {
        self.validate()?;
        self.registry.delegate(key, wait, cancelled)
    }
}

fn validate_anchor(fd: &OwnedFd) -> Result<()> {
    let metadata = fs::fstat(fd)?;
    let filesystem = fs::fstatfs(fd)?;
    let local = [
        libc::TMPFS_MAGIC,
        libc::EXT4_SUPER_MAGIC,
        libc::BTRFS_SUPER_MAGIC,
        libc::XFS_SUPER_MAGIC,
    ];
    if !local.contains(&filesystem.f_type) {
        return Err(Error::other(
            "native file registry requires a supported local filesystem",
        ));
    }
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
        || metadata.st_uid != 0
        || metadata.st_mode & 0o7777 != 0o1777
        || metadata.st_nlink == 0
    {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "native file registry needs root-owned sticky /var/tmp",
        ));
    }
    Ok(())
}
