use super::{Binding, Error, cancellation::Cancellation, config, random};
use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const LIMIT: u64 = 64 * 1024;
const WAIT: Duration = Duration::from_secs(2);

pub(super) struct Lease(File);

impl std::ops::Deref for Lease {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs::flock(&self.0, FlockOperation::Unlock);
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("Credential")
            .field("material", &"[redacted]")
            .finish()
    }
}

impl Credential {
    pub fn validate(&self) -> Result<(), Error> {
        let visible = |text: &str| {
            !text.is_empty()
                && text.len() <= 16384
                && text.bytes().all(|b| (0x21..=0x7e).contains(&b))
        };
        if !visible(&self.access_token)
            || self
                .refresh_token
                .as_ref()
                .is_some_and(|text| !visible(text))
            || self.scopes.len() > 32
            || !self.scopes.iter().all(|s| config::scope(s))
            || !self
                .access_token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
        {
            return Err(Error::Credential);
        }
        Ok(())
    }

    pub fn fresh(&self, now: u64) -> bool {
        self.expires_at.is_none_or(|expires| expires > now)
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub epoch: u64,
    pub refresh_after: Option<u64>,
    pub credential: Option<Credential>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u8,
    binding: Binding,
    generation: u64,
    #[serde(default)]
    epoch: Option<u64>,
    #[serde(default)]
    refresh_after: Option<u64>,
    credential: Option<Credential>,
}

pub struct Store {
    path: PathBuf,
    directory: File,
    binding: Binding,
    key: String,
}

fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn same(left: &fs::Stat, right: &fs::Stat) -> bool {
    (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)
}

fn regular(stat: &fs::Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
        && stat.st_uid == rustix::process::geteuid().as_raw()
        && stat.st_mode & 0o7777 == 0o600
        && stat.st_nlink == 1
}

fn directories(path: &Path, create: bool) -> Result<File, Error> {
    if !path.is_absolute() || path.as_os_str().len() > 4096 || path.components().count() > 128 {
        return Err(Error::Store);
    }
    let mut directory: File = fs::open("/", flags(), Mode::empty())
        .map_err(|_| Error::Store)?
        .into();
    for component in path.components() {
        let leaf = match component {
            Component::RootDir => continue,
            Component::Normal(leaf) => leaf,
            _ => return Err(Error::Store),
        };
        if create {
            match fs::mkdirat(&directory, leaf, Mode::RWXU) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(_) => return Err(Error::Store),
            }
        }
        directory = fs::openat(&directory, leaf, flags(), Mode::empty())
            .map_err(|_| Error::Store)?
            .into();
        let stat = fs::fstat(&directory).map_err(|_| Error::Store)?;
        let owned = stat.st_uid == rustix::process::geteuid().as_raw();
        let root_sticky = stat.st_uid == 0 && stat.st_mode & 0o1000 != 0;
        if (!owned && stat.st_uid != 0)
            || (stat.st_mode & 0o022 != 0 && !root_sticky)
            || stat.st_nlink == 0
        {
            return Err(Error::Store);
        }
    }
    let stat = fs::fstat(&directory).map_err(|_| Error::Store)?;
    if stat.st_uid != rustix::process::geteuid().as_raw() || stat.st_mode & 0o7777 != 0o700 {
        return Err(Error::Store);
    }
    Ok(directory)
}

impl Store {
    pub(super) fn matches(&self, binding: &Binding) -> bool {
        &self.binding == binding
    }
    pub fn at(path: &Path, binding: Binding) -> Result<Self, Error> {
        binding.validate()?;
        let directory = directories(path, true)?;
        let key = binding.key();
        Ok(Self {
            path: path.into(),
            directory,
            binding,
            key,
        })
    }

    pub fn open(binding: Binding) -> Result<Self, Error> {
        let root = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => {
                PathBuf::from(std::env::var_os("HOME").ok_or(Error::Store)?).join(".local/state")
            }
        };
        Self::at(&root.join("casper/mcp-auth"), binding)
    }

    fn validate(&self) -> Result<(), Error> {
        let named = directories(&self.path, false)?;
        if !same(
            &fs::fstat(&named).map_err(|_| Error::Store)?,
            &fs::fstat(&self.directory).map_err(|_| Error::Store)?,
        ) {
            return Err(Error::Store);
        }
        Ok(())
    }

    fn locked(&self) -> Result<Lease, Error> {
        self.locking(None)
    }

    fn locking(&self, cancelled: Option<&Cancellation>) -> Result<Lease, Error> {
        if let Some(cancelled) = cancelled {
            cancelled.check()?;
        }
        self.validate()?;
        let name = format!("{}.lock", self.key);
        let file: File = fs::openat(
            &self.directory,
            name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| Error::Store)?
        .into();
        let opened = fs::fstat(&file).map_err(|_| Error::Store)?;
        if !regular(&opened) {
            return Err(Error::Store);
        }
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(cancelled) = cancelled {
                cancelled.check()?;
            }
            match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(rustix::io::Errno::WOULDBLOCK) => {}
                Err(_) => return Err(Error::Store),
            }
            if Instant::now() >= deadline {
                return Err(Error::Busy);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let file = Lease(file);
        let named = fs::statat(&self.directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::Store)?;
        if !regular(&named) || !same(&opened, &named) {
            return Err(Error::Store);
        }
        self.validate()?;
        Ok(file)
    }

    fn lock_valid(&self, file: &File) -> Result<(), Error> {
        self.named_lock_valid(file, "lock")
    }

    pub(super) fn refresh_valid(&self, file: &File) -> Result<(), Error> {
        self.named_lock_valid(file, "refresh.lock")
    }

    pub(super) fn try_refresh(&self) -> Result<Option<Lease>, Error> {
        self.validate()?;
        let file: File = fs::openat(
            &self.directory,
            format!("{}.refresh.lock", self.key).as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| Error::Store)?
        .into();
        self.refresh_valid(&file)?;
        match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                let file = Lease(file);
                self.refresh_valid(&file)?;
                Ok(Some(file))
            }
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(_) => Err(Error::Store),
        }
    }

    fn named_lock_valid(&self, file: &File, suffix: &str) -> Result<(), Error> {
        self.validate()?;
        let held = fs::fstat(file).map_err(|_| Error::Store)?;
        let named = fs::statat(
            &self.directory,
            format!("{}.{suffix}", self.key).as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| Error::Store)?;
        if !regular(&held) || !regular(&named) || !same(&held, &named) {
            return Err(Error::Store);
        }
        Ok(())
    }

    fn read(&self) -> Result<Snapshot, Error> {
        let name = format!("{}.json", self.key);
        let file: File = match fs::openat(
            &self.directory,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(file) => file.into(),
            Err(rustix::io::Errno::NOENT) => {
                return Ok(Snapshot {
                    generation: 0,
                    epoch: 0,
                    refresh_after: None,
                    credential: None,
                });
            }
            Err(_) => return Err(Error::Store),
        };
        let opened = fs::fstat(&file).map_err(|_| Error::Store)?;
        if !regular(&opened) || opened.st_size < 0 || opened.st_size as u64 > LIMIT {
            return Err(Error::Store);
        }
        let mut bytes = Vec::new();
        (&file)
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Store)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Store);
        }
        let after = fs::fstat(&file).map_err(|_| Error::Store)?;
        let named = fs::statat(&self.directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::Store)?;
        if !regular(&after)
            || !regular(&named)
            || !same(&opened, &named)
            || !same(&opened, &after)
            || opened.st_size != after.st_size
            || opened.st_mtime != after.st_mtime
            || opened.st_mtime_nsec != after.st_mtime_nsec
            || opened.st_ctime != after.st_ctime
            || opened.st_ctime_nsec != after.st_ctime_nsec
        {
            return Err(Error::Store);
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| Error::Store)?;
        if !matches!(record.schema, 1 | 2)
            || record.binding != self.binding
            || (record.schema == 2 && record.epoch.is_none())
            || record.epoch.is_some_and(|epoch| epoch > record.generation)
        {
            return Err(Error::Store);
        }
        if let Some(credential) = &record.credential {
            credential.validate().map_err(|_| Error::Store)?;
        }
        Ok(Snapshot {
            generation: record.generation,
            epoch: record.epoch.unwrap_or(record.generation),
            refresh_after: record.refresh_after,
            credential: record.credential,
        })
    }

    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.snapshot_with(None)
    }

    pub(super) fn snapshot_cancellable(&self, cancelled: &Cancellation) -> Result<Snapshot, Error> {
        self.snapshot_with(Some(cancelled))
    }

    fn snapshot_with(&self, cancelled: Option<&Cancellation>) -> Result<Snapshot, Error> {
        let lock = match cancelled {
            Some(cancelled) => self.locking(Some(cancelled))?,
            None => self.locked()?,
        };
        let state = self.read()?;
        self.lock_valid(&lock)?;
        if let Some(cancelled) = cancelled {
            cancelled.check()?;
        }
        Ok(state)
    }

    pub fn replace(&self, expected: u64, credential: Credential) -> Result<u64, Error> {
        credential.validate()?;
        self.change(Some(expected), Some(credential), None, None, None)
    }

    pub fn logout(&self) -> Result<u64, Error> {
        self.change(None, None, None, None, None)
    }

    pub(super) fn replace_cancellable(
        &self,
        expected: u64,
        credential: Credential,
        cancelled: &Cancellation,
    ) -> Result<u64, Error> {
        credential.validate()?;
        self.change(
            Some(expected),
            Some(credential),
            None,
            None,
            Some(cancelled),
        )
    }

    pub(super) fn rotate(
        &self,
        expected: &Snapshot,
        credential: Credential,
        lease: &File,
        cancelled: &Cancellation,
    ) -> Result<u64, Error> {
        credential.validate()?;
        self.refresh_valid(lease)?;
        self.change(
            Some(expected.generation),
            Some(credential),
            Some(expected.epoch),
            None,
            Some(cancelled),
        )
    }

    pub(super) fn begin_refresh(
        &self,
        expected: &Snapshot,
        lease: &File,
        now: u64,
        cancelled: &Cancellation,
    ) -> Result<Snapshot, Error> {
        self.refresh_valid(lease)?;
        if expected.refresh_after.is_some_and(|after| after > now) {
            return Err(Error::Backoff);
        }
        let credential = expected.credential.clone().ok_or(Error::Required)?;
        self.change(
            Some(expected.generation),
            Some(credential),
            Some(expected.epoch),
            Some(now.checked_add(30).ok_or(Error::Store)?),
            Some(cancelled),
        )?;
        let state = self.snapshot_cancellable(cancelled)?;
        if state.epoch != expected.epoch || state.credential.is_none() {
            return Err(Error::Changed);
        }
        Ok(state)
    }

    fn change(
        &self,
        expected: Option<u64>,
        credential: Option<Credential>,
        epoch: Option<u64>,
        refresh_after: Option<u64>,
        cancelled: Option<&Cancellation>,
    ) -> Result<u64, Error> {
        let lock = self.locking(cancelled)?;
        if let Some(cancelled) = cancelled {
            cancelled.check()?;
        }
        let state = self.read()?;
        if expected.is_some_and(|value| value != state.generation) {
            return Err(Error::Changed);
        }
        if epoch.is_some_and(|value| value != state.epoch || state.credential.is_none()) {
            return Err(Error::Changed);
        }
        let generation = state.generation.checked_add(1).ok_or(Error::Store)?;
        let bytes = serde_json::to_vec(&Record {
            schema: 2,
            binding: self.binding.clone(),
            generation,
            epoch: Some(epoch.unwrap_or(generation)),
            refresh_after,
            credential,
        })
        .map_err(|_| Error::Store)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Store);
        }
        let temporary = format!(".{}.{}.tmp", self.key, random()?);
        let mut file: File = fs::openat(
            &self.directory,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| Error::Store)?
        .into();
        let saved = (|| {
            file.write_all(&bytes).map_err(|_| Error::Store)?;
            file.sync_all().map_err(|_| Error::Store)?;
            let stat = fs::fstat(&file).map_err(|_| Error::Store)?;
            let named = fs::statat(
                &self.directory,
                temporary.as_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| Error::Store)?;
            if !regular(&stat) || !regular(&named) || !same(&stat, &named) {
                return Err(Error::Store);
            }
            self.lock_valid(&lock)?;
            if let Some(cancelled) = cancelled {
                cancelled.check()?;
            }
            fs::renameat(
                &self.directory,
                temporary.as_str(),
                &self.directory,
                format!("{}.json", self.key).as_str(),
            )
            .map_err(|_| Error::Store)?;
            self.directory.sync_all().map_err(|_| Error::Store)?;
            self.lock_valid(&lock)?;
            Ok(generation)
        })();
        if saved.is_err() {
            let _ = fs::unlinkat(&self.directory, temporary.as_str(), AtFlags::empty());
        }
        saved
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fault_tests;
