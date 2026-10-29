use crate::files::locking::{Binding, Key};
use rustix::fs::{self, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Error, ErrorKind, Read, Result};
use std::os::fd::OwnedFd;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision_contract: u16,
    pub path_key: String,
    pub revision: String,
    pub coverage: String,
    pub source_text: String,
    pub source_bytes: usize,
}

pub(super) struct Resolved {
    pub(super) binding: Binding,
    pub(super) parent: OwnedFd,
    named_parent: String,
    pub(super) leaf: String,
}

pub(super) struct Acquired {
    pub snapshot: Snapshot,
    pub file: std::fs::File,
    pub metadata: fs::Stat,
}

impl Resolved {
    pub(super) fn open(path: &str) -> Result<Self> {
        Self::resolve(path, false)
    }

    pub(super) fn bind(path: &str) -> Result<Self> {
        let (parent_path, leaf) = path.rsplit_once('/').map_or((".", path), |(parent, leaf)| {
            (if parent.is_empty() { "/" } else { parent }, leaf)
        });
        let parent = directory(parent_path)?;
        let binding = Binding::from_parent(parent.try_clone()?, std::ffi::OsStr::new(leaf))?;
        Ok(Self {
            binding,
            parent,
            named_parent: parent_path.into(),
            leaf: leaf.into(),
        })
    }

    pub(super) fn resolve(path: &str, creation: bool) -> Result<Self> {
        let resolved = Self::bind(path)?;
        let leaf = resolved.leaf.as_str();
        let metadata = match fs::statat(&resolved.parent, leaf, fs::AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) if creation => return Ok(resolved),
            result => result?,
        };
        if creation {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "native creation target already exists",
            ));
        }
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
            || metadata.st_nlink != 1
        {
            return Err(invalid(
                "native snapshots require a single-linked regular file without a leaf symlink",
            ));
        }
        if metadata.st_size < 0 || metadata.st_size as u64 >= crate::source::EDIT_BYTES as u64 {
            return Err(invalid("editable text must be smaller than 1 MiB"));
        }
        Ok(resolved)
    }

    pub(super) fn validate_parent(&self) -> Result<()> {
        self.binding.validate()?;
        let named = directory(&self.named_parent)?;
        let actual = fs::fstat(&self.parent)?;
        let named = fs::fstat(&named)?;
        if (actual.st_dev, actual.st_ino) != (named.st_dev, named.st_ino) {
            return Err(invalid("native file parent binding changed"));
        }
        Ok(())
    }

    pub(super) fn capture(&self) -> Result<Snapshot> {
        self.acquire(false).map(|value| value.snapshot)
    }

    pub(super) fn acquire(&self, writable: bool) -> Result<Acquired> {
        self.validate_parent()?;
        let fd = fs::openat(
            &self.parent,
            self.leaf.as_str(),
            (if writable {
                OFlags::RDWR
            } else {
                OFlags::RDONLY
            }) | OFlags::NONBLOCK
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let before = fs::fstat(&fd)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile || before.st_nlink != 1
        {
            return Err(invalid(
                "native snapshots require a single-linked regular file without a leaf symlink",
            ));
        }
        if before.st_size < 0 || before.st_size as u64 >= crate::source::EDIT_BYTES as u64 {
            return Err(invalid("editable text must be smaller than 1 MiB"));
        }
        let mut file = std::fs::File::from(fd);
        let mut bytes = Vec::new();
        (&mut file)
            .take(crate::source::EDIT_BYTES as u64)
            .read_to_end(&mut bytes)?;
        let after = fs::fstat(&file)?;
        let named = fs::statat(
            &self.parent,
            self.leaf.as_str(),
            fs::AtFlags::SYMLINK_NOFOLLOW,
        )?;
        if fingerprint(&before) != fingerprint(&after)
            || fingerprint(&after) != fingerprint(&named)
            || bytes.len() as u64 != after.st_size as u64
        {
            return Err(invalid("native source changed during snapshot acquisition"));
        }
        self.validate_parent()?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid("editable text must be UTF-8 without NUL"))?;
        if bytes.contains(&0) {
            return Err(invalid("editable text must be UTF-8 without NUL"));
        }
        Ok(Acquired {
            file,
            metadata: after,
            snapshot: Snapshot {
                revision_contract: 1,
                path_key: self.binding.key().into(),
                revision: revision(self.binding.key(), &after, &bytes),
                coverage: "snapshot".into(),
                source_text: text.to_owned(),
                source_bytes: bytes.len(),
            },
        })
    }
}

impl Snapshot {
    pub(super) fn validate(&self, key: &Key) -> Result<()> {
        if self.revision_contract != 1
            || self.path_key != key.as_str()
            || self.coverage != "snapshot"
            || self.source_bytes != self.source_text.len()
            || self.source_bytes >= crate::source::EDIT_BYTES
            || self.source_text.as_bytes().contains(&0)
            || self.revision.len() != 69
            || !self.revision.starts_with("cfr1:")
        {
            return Err(invalid(
                "native snapshot reply is incomplete or unsupported",
            ));
        }
        Key::from_wire(
            self.revision
                .get(5..)
                .ok_or_else(|| invalid("invalid native revision"))?,
        )?;
        Ok(())
    }
}

pub(super) fn fingerprint(stat: &fs::Stat) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&stat.st_dev.to_le_bytes());
    bytes.extend_from_slice(&stat.st_ino.to_le_bytes());
    bytes.extend_from_slice(&stat.st_size.to_le_bytes());
    bytes.extend_from_slice(&stat.st_mode.to_le_bytes());
    bytes.extend_from_slice(&stat.st_uid.to_le_bytes());
    bytes.extend_from_slice(&stat.st_gid.to_le_bytes());
    bytes.extend_from_slice(&stat.st_nlink.to_le_bytes());
    bytes.extend_from_slice(&stat.st_mtime.to_le_bytes());
    bytes.extend_from_slice(&stat.st_mtime_nsec.to_le_bytes());
    bytes.extend_from_slice(&stat.st_ctime.to_le_bytes());
    bytes.extend_from_slice(&stat.st_ctime_nsec.to_le_bytes());
    bytes
}

pub(super) fn revision(key: &str, metadata: &fs::Stat, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"casper:source-revision:1\0");
    digest.update(key.as_bytes());
    digest.update(fingerprint(metadata));
    digest.update(bytes);
    format!("cfr1:{:x}", digest.finalize())
}

fn directory(path: &str) -> Result<OwnedFd> {
    Ok(fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn content_changes_revision_with_identical_metadata_and_atime_is_not_a_revision() {
        let root = crate::scratch::Scratch::new("native-revision", "content");
        let file = std::fs::File::create_new(root.join("source")).expect("fixture");
        let stat = fs::fstat(&file).expect("metadata");
        let first = revision("binding", &stat, b"one\r\n");
        assert_ne!(first, revision("binding", &stat, b"two\r\n"));
        assert_ne!(first, revision("binding", &stat, b"one\n"));
        assert_ne!(first, revision("other", &stat, b"one\r\n"));
        let mut access_only = stat;
        access_only.st_atime = access_only.st_atime.saturating_add(1);
        access_only.st_atime_nsec = access_only.st_atime_nsec.saturating_add(1);
        assert_eq!(first, revision("binding", &access_only, b"one\r\n"));
    }

    #[test]
    fn incomplete_or_unsupported_native_snapshot_envelopes_refuse() {
        let key = Key::from_wire(&"0".repeat(64)).expect("key");
        let original = serde_json::json!({"revision_contract":1,"path_key":key.as_str(),
            "revision":format!("cfr1:{}", "0".repeat(64)),"coverage":"snapshot",
            "source_text":"ok","source_bytes":2});
        let valid: Snapshot = serde_json::from_value(original.clone()).expect("golden envelope");
        valid.validate(&key).expect("complete envelope");
        for (field, value) in [
            ("revision_contract", serde_json::json!(2)),
            ("path_key", serde_json::json!("other")),
            ("coverage", serde_json::json!("derived")),
            ("source_bytes", serde_json::json!(1)),
            ("source_text", serde_json::json!("o\0")),
            ("revision", serde_json::json!("cfr2:unknown")),
        ] {
            let mut changed = original.clone();
            changed
                .as_object_mut()
                .expect("envelope")
                .insert(field.into(), value);
            let decoded: Snapshot = serde_json::from_value(changed).expect("typed envelope");
            assert!(decoded.validate(&key).is_err(), "accepted {field}");
        }
        let bytes = serde_json::to_vec(&original).expect("golden bytes");
        for end in 0..bytes.len() {
            assert!(serde_json::from_slice::<Snapshot>(bytes.get(..end).expect("prefix")).is_err());
        }
    }
}
