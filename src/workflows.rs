//! Fixed-depth, descriptor-bound workflow source acquisition.

mod process;
pub mod protocol;
mod reading;
pub use process::invoke;
#[cfg(test)]
mod tests;

use rustix::fs::{self, Mode, OFlags, ResolveFlags};
use serde::{Deserialize, Serialize};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

pub const FILE_MOST: usize = 64 * 1024;
pub const METADATA_MOST: usize = 4 * 1024;
pub const ENTRIES_MOST: usize = 256;
pub const ACQUISITION_TIME: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    #[error("workflow root or name is invalid")]
    InvalidPath,
    #[error("workflow source was not found")]
    NotFound,
    #[error("workflow source is unreadable")]
    Unreadable,
    #[error("workflow source must be a single-linked regular file without symlinks")]
    UnsafePath,
    #[error("workflow source changed during acquisition")]
    Changed,
    #[error("workflow root exceeds 256 entries")]
    TooManyEntries,
    #[error("workflow source exceeds 64 KiB")]
    Oversized,
    #[error("workflow metadata is missing or exceeds 4 KiB")]
    Metadata,
    #[error("workflow text is not UTF-8 without NUL")]
    Encoding,
    #[error("workflow acquisition deadline elapsed")]
    Timeout,
    #[error("workflow protocol is malformed or unsupported")]
    Protocol,
    #[error("workflow worker could not be contained or started")]
    Worker,
    #[error("workflow acquisition was cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub entries: Vec<Candidate>,
    pub invalid_names: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub name: String,
    pub metadata: Result<String, Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub text: String,
    pub sha256: String,
}

pub fn catalog(path: &Path) -> Result<Catalog, Failure> {
    let budget = Budget::new();
    let root = match Root::open(path, &budget) {
        Err(Failure::NotFound) => {
            return Ok(Catalog {
                entries: Vec::new(),
                invalid_names: 0,
            });
        }
        other => other?,
    };
    catalog_root(&root, &budget)
}

fn catalog_root(root: &Root<'_>, budget: &Budget) -> Result<Catalog, Failure> {
    let before = fs::fstat(&root.fd).map_err(io)?;
    let mut names = Vec::new();
    let mut invalid_names = 0;
    let mut count = 0;
    for entry in fs::Dir::read_from(&root.fd).map_err(io)? {
        budget.check()?;
        let entry = entry.map_err(io)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        count += 1;
        if count > ENTRIES_MOST {
            return Err(Failure::TooManyEntries);
        }
        match std::str::from_utf8(bytes) {
            Ok(name) if valid_name(name) => names.push(name.to_owned()),
            _ => invalid_names += 1,
        }
    }
    names.sort();
    let mut entries = Vec::new();
    for name in names {
        budget.check()?;
        let metadata = reading::read(root, &name, true, budget);
        if matches!(metadata, Err(Failure::Timeout | Failure::Changed)) {
            return Err(metadata.err().ok_or(Failure::Changed)?);
        }
        entries.push(Candidate { name, metadata });
    }
    root.validate(budget)?;
    if reading::stamp(&before) != reading::stamp(&fs::fstat(&root.fd).map_err(io)?) {
        return Err(Failure::Changed);
    }
    Ok(Catalog {
        entries,
        invalid_names,
    })
}

pub fn load(path: &Path, name: &str) -> Result<Source, Failure> {
    let budget = Budget::new();
    let root = Root::open(path, &budget)?;
    load_root(&root, name, &budget)
}

fn load_root(root: &Root<'_>, name: &str, budget: &Budget) -> Result<Source, Failure> {
    use sha2::{Digest, Sha256};
    let text = reading::read(root, name, false, budget)?;
    let sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    budget.check()?;
    Ok(Source { text, sha256 })
}

struct Budget(Instant);

impl Budget {
    fn new() -> Self {
        Self(Instant::now() + ACQUISITION_TIME)
    }

    fn check(&self) -> Result<(), Failure> {
        if Instant::now() >= self.0 {
            Err(Failure::Timeout)
        } else {
            Ok(())
        }
    }
}

struct Root<'a> {
    path: &'a Path,
    fd: OwnedFd,
}

impl<'a> Root<'a> {
    fn open(path: &'a Path, budget: &Budget) -> Result<Self, Failure> {
        budget.check()?;
        let bytes = path.as_os_str().as_bytes();
        if !path.is_absolute()
            || bytes.len() > 4096
            || bytes.contains(&0)
            || path.components().any(|p| matches!(p, Component::ParentDir))
            || path.components().count() > 256
        {
            return Err(Failure::InvalidPath);
        }
        let fd = fs::openat2(
            fs::CWD,
            path,
            directory_flags(),
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .map_err(io)?;
        budget.check()?;
        Ok(Self { path, fd })
    }

    fn validate(&self, budget: &Budget) -> Result<(), Failure> {
        let current = Self::open(self.path, budget)?;
        same_directory(&self.fd, &current.fd)
    }
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

fn same_directory(a: &OwnedFd, b: &OwnedFd) -> Result<(), Failure> {
    let a = fs::fstat(a).map_err(io)?;
    let b = fs::fstat(b).map_err(io)?;
    if (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino) {
        Ok(())
    } else {
        Err(Failure::Changed)
    }
}

fn valid_name(name: &str) -> bool {
    let alphanumeric = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| alphanumeric(b) || b == b'-')
        && name.bytes().next().is_some_and(alphanumeric)
        && name.bytes().next_back().is_some_and(alphanumeric)
}

fn io(error: rustix::io::Errno) -> Failure {
    match error {
        rustix::io::Errno::NOENT => Failure::NotFound,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Failure::UnsafePath,
        _ => Failure::Unreadable,
    }
}
