use super::snapshot::Acquired;
use rustix::fs::{self, FileType, Mode, OFlags};
use std::io::{Error, ErrorKind, Result, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};

static NAMES: AtomicU64 = AtomicU64::new(0);

pub(super) struct Stage<'a> {
    parent: &'a OwnedFd,
    file: std::fs::File,
    linked: Option<String>,
}

impl<'a> Stage<'a> {
    pub(super) fn prepare(
        parent: &'a OwnedFd,
        source: Option<&Acquired>,
        text: &str,
    ) -> Result<Self> {
        let filesystem = fs::fstatfs(parent)?;
        if ![
            libc::TMPFS_MAGIC,
            libc::EXT4_SUPER_MAGIC,
            libc::BTRFS_SUPER_MAGIC,
            libc::XFS_SUPER_MAGIC,
        ]
        .contains(&filesystem.f_type)
        {
            return Err(invalid(
                "native atomic mutation requires a supported local filesystem",
            ));
        }
        match fs::fgetxattr(parent, "system.posix_acl_default", &mut [0u8; 0][..]) {
            Err(rustix::io::Errno::NODATA) => {}
            _ => {
                return Err(invalid(
                    "native atomic mutation refuses inherited default ACLs or unverifiable ACL metadata",
                ));
            }
        }
        if let Some(source) = source {
            source_metadata(source)?;
        }
        let mode = if source.is_some() {
            Mode::RUSR | Mode::WUSR
        } else {
            Mode::from_raw_mode(0o666)
        };
        let fd = fs::openat(
            parent,
            ".",
            OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC,
            mode,
        )
        .map_err(|e| Error::other(format!("anonymous native staging is unavailable: {e}")))?;
        let mut stage = Self {
            parent,
            file: std::fs::File::from(fd),
            linked: None,
        };
        no_xattrs(&stage.file)?;
        stage.file.write_all(text.as_bytes())?;
        if let Some(source) = source {
            let metadata = fs::fstat(&stage.file)?;
            if metadata.st_gid != source.metadata.st_gid {
                fs::fchown(
                    &stage.file,
                    None,
                    Some(rustix::process::Gid::from_raw(source.metadata.st_gid)),
                )
                .map_err(|e| {
                    Error::other(format!(
                        "native mutation cannot preserve group ownership: {e}"
                    ))
                })?;
            }
            fs::fchmod(
                &stage.file,
                Mode::from_raw_mode(source.metadata.st_mode & 0o777),
            )?;
            let staged = fs::fstat(&stage.file)?;
            if staged.st_uid != source.metadata.st_uid
                || staged.st_gid != source.metadata.st_gid
                || staged.st_mode & 0o7777 != source.metadata.st_mode & 0o7777
            {
                return Err(invalid(
                    "native mutation cannot preserve exact ownership and mode",
                ));
            }
        }
        no_xattrs(&stage.file)?;
        if fs::fstat(&stage.file)?.st_size != text.len() as i64 {
            return Err(invalid("native stage is incomplete"));
        }
        stage.file.sync_all()?;
        Ok(stage)
    }

    pub(super) fn link_for_replace(&mut self) -> Result<()> {
        let inode = fs::fstat(&self.file)?.st_ino;
        for _ in 0..32 {
            let name = format!(
                ".casper-stage-{inode:x}-{:x}-{:x}",
                std::process::id(),
                NAMES.fetch_add(1, Ordering::Relaxed)
            );
            match self.link(&name) {
                Ok(()) => {
                    self.linked = Some(name);
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(invalid("native stage name collision bound exceeded"))
    }

    fn link(&self, name: &str) -> Result<()> {
        Ok(fs::linkat(
            fs::CWD,
            format!("/proc/self/fd/{}", self.file.as_raw_fd()),
            self.parent,
            name,
            fs::AtFlags::SYMLINK_FOLLOW,
        )?)
    }

    pub(super) fn commit(&mut self, leaf: &str, creation: bool) -> Result<()> {
        if creation {
            return self.link(leaf);
        }
        let name = self
            .linked
            .as_deref()
            .ok_or_else(|| invalid("native replacement stage has no binding"))?;
        if !self.owns(name)? {
            return Err(invalid("native replacement stage binding changed"));
        }
        fs::renameat(self.parent, name, self.parent, leaf)?;
        self.linked = None;
        Ok(())
    }

    fn owns(&self, name: &str) -> Result<bool> {
        let opened = fs::fstat(&self.file)?;
        let named = fs::statat(self.parent, name, fs::AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(
            FileType::from_raw_mode(named.st_mode) == FileType::RegularFile
                && (opened.st_dev, opened.st_ino) == (named.st_dev, named.st_ino)
                && opened.st_nlink == 1
                && named.st_nlink == 1,
        )
    }

    pub(super) fn matches(&self, source: &Acquired) -> Result<bool> {
        let metadata = fs::fstat(&self.file)?;
        Ok((metadata.st_dev, metadata.st_ino) == (source.metadata.st_dev, source.metadata.st_ino))
    }

    #[cfg(test)]
    pub(super) fn identity(&self) -> Result<(u64, u64)> {
        let metadata = fs::fstat(&self.file)?;
        Ok((metadata.st_dev, metadata.st_ino))
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if let Some(name) = self.linked.as_deref()
            && self.owns(name).unwrap_or(false)
        {
            let _ = fs::unlinkat(self.parent, name, fs::AtFlags::empty());
        }
    }
}

fn source_metadata(source: &Acquired) -> Result<()> {
    let metadata = &source.metadata;
    if metadata.st_uid != rustix::process::geteuid().as_raw()
        || metadata.st_gid == u32::MAX
        || metadata.st_mode & 0o7000 != 0
        || metadata.st_mode & 0o200 == 0
    {
        return Err(invalid(
            "native atomic mutation requires an owner-writable file without privileged mode bits",
        ));
    }
    no_xattrs(&source.file)
}

fn no_xattrs(fd: impl AsFd) -> Result<()> {
    match fs::flistxattr(fd, &mut [0u8; 0][..]) {
        Ok(0) => Ok(()),
        _ => Err(invalid(
            "native atomic mutation refuses ACLs, xattrs or unverifiable extended metadata",
        )),
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod tests;
