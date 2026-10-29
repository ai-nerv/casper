use super::{
    Budget, FILE_MOST, Failure, METADATA_MOST, Root, directory_flags, io, same_directory,
    valid_name,
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use std::io::Read as _;

pub(super) fn read(
    root: &Root<'_>,
    name: &str,
    metadata: bool,
    budget: &Budget,
) -> Result<String, Failure> {
    capture(root, name, metadata, budget, || {})
}

pub(super) fn capture(
    root: &Root<'_>,
    name: &str,
    metadata: bool,
    budget: &Budget,
    opened_hook: impl FnOnce(),
) -> Result<String, Failure> {
    budget.check()?;
    if !valid_name(name) {
        return Err(Failure::InvalidPath);
    }
    let directory = fs::openat(&root.fd, name, directory_flags(), Mode::empty()).map_err(io)?;
    let before = fs::statat(&directory, "SKILL.md", AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
    regular(&before)?;
    let fd = fs::openat(
        &directory,
        "SKILL.md",
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io)?;
    let opened = fs::fstat(&fd).map_err(io)?;
    regular(&opened)?;
    if stamp(&before) != stamp(&opened) {
        return Err(Failure::Changed);
    }
    opened_hook();
    budget.check()?;
    let mut file = std::fs::File::from(fd);
    let cap = if metadata { METADATA_MOST } else { FILE_MOST };
    let mut bytes = Vec::new();
    (&mut file)
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Unreadable)?;
    let after = fs::fstat(&file).map_err(io)?;
    let named = fs::statat(&directory, "SKILL.md", AtFlags::SYMLINK_NOFOLLOW).map_err(io)?;
    if stamp(&before) != stamp(&after)
        || stamp(&after) != stamp(&named)
        || bytes.len() != (after.st_size as usize).min(cap + 1)
    {
        return Err(Failure::Changed);
    }
    root.validate(budget)?;
    let named_directory =
        fs::openat(&root.fd, name, directory_flags(), Mode::empty()).map_err(io)?;
    same_directory(&directory, &named_directory)?;
    if metadata {
        bytes.truncate(metadata_end(&bytes)?);
    }
    if bytes.len() > cap {
        return Err(Failure::Oversized);
    }
    if bytes.contains(&0) {
        return Err(Failure::Encoding);
    }
    let text = String::from_utf8(bytes).map_err(|_| Failure::Encoding)?;
    budget.check()?;
    Ok(text)
}

fn regular(stat: &fs::Stat) -> Result<(), Failure> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(Failure::UnsafePath);
    }
    if stat.st_size < 0 || stat.st_size as u64 > FILE_MOST as u64 {
        return Err(Failure::Oversized);
    }
    Ok(())
}

fn metadata_end(bytes: &[u8]) -> Result<usize, Failure> {
    let mut lines = bytes.split_inclusive(|b| *b == b'\n');
    let delimiter = |raw: &[u8]| {
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        raw.strip_suffix(b"\r").unwrap_or(raw) == b"---"
    };
    let first = lines.next().ok_or(Failure::Metadata)?;
    if !delimiter(first) {
        return Err(Failure::Metadata);
    }
    let mut end = first.len();
    for line in lines {
        end += line.len();
        if end > METADATA_MOST {
            return Err(Failure::Metadata);
        }
        if delimiter(line) {
            return Ok(end);
        }
    }
    Err(Failure::Metadata)
}

pub(super) fn stamp(stat: &fs::Stat) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&stat.st_dev.to_le_bytes());
    bytes.extend_from_slice(&stat.st_ino.to_le_bytes());
    bytes.extend_from_slice(&stat.st_nlink.to_le_bytes());
    bytes.extend_from_slice(&stat.st_size.to_le_bytes());
    bytes.extend_from_slice(&stat.st_mode.to_le_bytes());
    bytes.extend_from_slice(&stat.st_uid.to_le_bytes());
    bytes.extend_from_slice(&stat.st_gid.to_le_bytes());
    bytes.extend_from_slice(&stat.st_mtime.to_le_bytes());
    bytes.extend_from_slice(&stat.st_mtime_nsec.to_le_bytes());
    bytes.extend_from_slice(&stat.st_ctime.to_le_bytes());
    bytes.extend_from_slice(&stat.st_ctime_nsec.to_le_bytes());
    bytes
}
