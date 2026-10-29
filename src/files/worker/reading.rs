use super::snapshot::{Resolved, Snapshot, fingerprint, revision};
use crate::files::locking::Key;
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind, Read, Result};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reading {
    pub revision_contract: u16,
    pub path_key: String,
    pub text: String,
    pub source_bytes: usize,
    pub total_lines: usize,
    pub first_line: usize,
    pub last_line: usize,
    pub omitted_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
}

impl Resolved {
    pub(super) fn read(&self, offset: usize, limit: usize) -> Result<Reading> {
        self.read_with(offset, limit, || Ok(()))
    }

    fn read_with(
        &self,
        offset: usize,
        limit: usize,
        acquired: impl FnOnce() -> Result<()>,
    ) -> Result<Reading> {
        self.validate_parent()?;
        let named = fs::statat(&self.parent, self.leaf.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
        let target = fs::statat(&self.parent, self.leaf.as_str(), AtFlags::empty())?;
        if FileType::from_raw_mode(target.st_mode) != FileType::RegularFile {
            return Err(invalid("native text read requires a regular file"));
        }
        let fd = fs::openat(
            &self.parent,
            self.leaf.as_str(),
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let before = fs::fstat(&fd)?;
        if fingerprint(&before) != fingerprint(&target) {
            return Err(invalid("native read target changed while opening"));
        }
        let editable = FileType::from_raw_mode(named.st_mode) == FileType::RegularFile
            && before.st_nlink == 1
            && fingerprint(&before) == fingerprint(&named)
            && before.st_size >= 0
            && (before.st_size as u64) < crate::source::EDIT_BYTES as u64;
        let mut file = std::fs::File::from(fd);
        let scanned = scan(&mut file, offset, limit, editable)?;
        acquired()?;
        let after = fs::fstat(&file)?;
        let target_after = fs::statat(&self.parent, self.leaf.as_str(), AtFlags::empty())?;
        let named_after = fs::statat(&self.parent, self.leaf.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
        if fingerprint(&before) != fingerprint(&after)
            || fingerprint(&after) != fingerprint(&target_after)
            || fingerprint(&named) != fingerprint(&named_after)
        {
            return Err(invalid("native source changed during read acquisition"));
        }
        self.validate_parent()?;
        let snapshot = scanned
            .source
            .filter(|bytes| bytes.len() as u64 == after.st_size as u64 && !bytes.contains(&0))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| Snapshot {
                revision_contract: 1,
                path_key: self.binding.key().into(),
                revision: revision(self.binding.key(), &after, text.as_bytes()),
                coverage: "snapshot".into(),
                source_bytes: text.len(),
                source_text: text,
            });
        Ok(Reading {
            revision_contract: 1,
            path_key: self.binding.key().into(),
            text: String::from_utf8_lossy(&scanned.held).into_owned(),
            source_bytes: scanned.bytes,
            total_lines: scanned.lines,
            first_line: offset,
            last_line: offset
                .saturating_sub(1)
                .saturating_add(limit)
                .min(scanned.lines),
            omitted_bytes: scanned.selected - scanned.held.len(),
            snapshot,
        })
    }
}

impl Reading {
    pub fn presentation(&self) -> crate::tools::ReadText {
        crate::tools::ReadText {
            source_text: self
                .snapshot
                .as_ref()
                .map(|value| value.source_text.clone()),
            text: self.text.clone(),
            source_bytes: self.source_bytes,
            total_lines: self.total_lines,
            first_line: self.first_line,
            last_line: self.last_line,
            omitted_bytes: self.omitted_bytes,
        }
    }

    pub(super) fn validate(&self, key: &Key, offset: usize, limit: usize) -> Result<()> {
        if self.revision_contract != 1
            || self.path_key != key.as_str()
            || self.first_line != offset
            || self.last_line
                != offset
                    .saturating_sub(1)
                    .saturating_add(limit)
                    .min(self.total_lines)
            || self.total_lines > self.source_bytes
            || self.omitted_bytes > self.source_bytes
            || self.text.len() > crate::running::MOST * 3
        {
            return Err(invalid(
                "native read reply has an unsupported version, range or binding",
            ));
        }
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate(key)?;
            if snapshot.source_bytes != self.source_bytes {
                return Err(invalid(
                    "native read snapshot size does not match acquisition",
                ));
            }
            let scan = scan(snapshot.source_text.as_bytes(), offset, limit, false)?;
            if self.total_lines != scan.lines
                || self.text != String::from_utf8_lossy(&scan.held)
                || self.omitted_bytes != scan.selected - scan.held.len()
            {
                return Err(invalid(
                    "native read display differs from its complete snapshot",
                ));
            }
        }
        Ok(())
    }
}

struct Scanned {
    held: Vec<u8>,
    source: Option<Vec<u8>>,
    bytes: usize,
    lines: usize,
    selected: usize,
}

fn scan(mut input: impl Read, offset: usize, limit: usize, editable: bool) -> Result<Scanned> {
    let mut result = Scanned {
        held: Vec::new(),
        source: editable.then(Vec::new),
        bytes: 0,
        lines: 0,
        selected: 0,
    };
    let mut buffer = [0; 8192];
    let mut dangling = false;
    let mut selected_ends_newline = false;
    loop {
        let count = match input.read(&mut buffer) {
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            other => other?,
        };
        if count == 0 {
            break;
        }
        let bytes = buffer
            .get(..count)
            .ok_or_else(|| invalid("invalid native read size"))?;
        result.bytes = result
            .bytes
            .checked_add(count)
            .ok_or_else(|| invalid("native read size overflow"))?;
        if result.bytes >= crate::source::EDIT_BYTES {
            result.source = None;
        } else if let Some(source) = &mut result.source {
            source.extend_from_slice(bytes);
        }
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            let line = result
                .lines
                .checked_add(1)
                .ok_or_else(|| invalid("native line count overflow"))?;
            if line >= offset && line - offset < limit {
                result.selected = result
                    .selected
                    .checked_add(part.len())
                    .ok_or_else(|| invalid("native selection size overflow"))?;
                result
                    .held
                    .extend(part.iter().take(crate::running::MOST - result.held.len()));
                selected_ends_newline = part.last() == Some(&b'\n');
            }
            dangling = part.last() != Some(&b'\n');
            if !dangling {
                result.lines = line;
            }
        }
    }
    result.lines = result
        .lines
        .checked_add(usize::from(dangling))
        .ok_or_else(|| invalid("native line count overflow"))?;
    if selected_ends_newline {
        if result.selected == result.held.len() {
            result.held.pop();
        }
        result.selected -= 1;
    }
    if result.selected > result.held.len() {
        let mut from = 0;
        while let Some(bytes) = result.held.get(from..) {
            match std::str::from_utf8(bytes) {
                Ok(_) => break,
                Err(error) => {
                    from += error.valid_up_to();
                    if let Some(size) = error.error_len() {
                        from += size;
                    } else {
                        result.held.truncate(from);
                        break;
                    }
                }
            }
        }
    }
    Ok(result)
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "reading/tests.rs"]
mod tests;
