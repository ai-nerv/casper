//! Bounded metadata inspection of credential stores and their aliases.

use std::collections::BTreeSet;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 16_384;
const MAX_DEPTH: usize = 128;

pub(super) fn opened_path(fd: &OwnedFd) -> std::io::Result<PathBuf> {
    let actual = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))?;
    if !actual.is_absolute()
        || actual
            .as_os_str()
            .as_encoded_bytes()
            .ends_with(b" (deleted)")
    {
        return Err(std::io::Error::other("opened path has no stable name"));
    }
    Ok(actual)
}

pub(super) fn expand(roots: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
    let mut scan = Scan {
        roots,
        aliases: BTreeSet::new(),
        visited: BTreeSet::new(),
        remaining: MAX_ENTRIES,
    };
    for root in roots {
        scan.visit(root, 0)?;
    }
    Ok(scan.aliases.into_iter().collect())
}

struct Scan<'a> {
    roots: &'a [PathBuf],
    aliases: BTreeSet<PathBuf>,
    visited: BTreeSet<(u64, u64)>,
    remaining: usize,
}

impl Scan<'_> {
    fn visit(&mut self, path: &Path, depth: usize) -> std::io::Result<()> {
        if depth > MAX_DEPTH || self.remaining == 0 {
            return Err(std::io::Error::other(
                "credential metadata inspection limit exceeded",
            ));
        }
        self.remaining -= 1;
        let fd = match rustix::fs::open(
            path,
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT)
                if depth == 0
                    && std::fs::symlink_metadata(path)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let actual = opened_path(&fd)?;
        if !self.roots.iter().any(|root| actual.starts_with(root)) {
            self.aliases.insert(actual);
        }
        let metadata = rustix::fs::fstat(&fd)?;
        let kind = rustix::fs::FileType::from_raw_mode(metadata.st_mode);
        if kind == rustix::fs::FileType::RegularFile && metadata.st_nlink > 1 {
            return Err(std::io::Error::other(
                "hardlinked credential files cannot be isolated",
            ));
        }
        if !self.visited.insert((metadata.st_dev, metadata.st_ino))
            || kind != rustix::fs::FileType::Directory
        {
            return Ok(());
        }
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", fd.as_raw_fd()))? {
            self.visit(&entry?.path(), depth + 1)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_roots_are_allowed_but_dangling_nested_links_are_not() {
        let dir = crate::scratch::Scratch::new("credentials", "dangling");
        let root = dir.join("store");
        assert!(
            expand(std::slice::from_ref(&root))
                .expect("missing root")
                .is_empty()
        );
        std::fs::create_dir(&root).expect("store");
        std::os::unix::fs::symlink(dir.join("missing"), root.join("key")).expect("dangling link");
        assert!(expand(&[root]).is_err());
    }

    #[test]
    fn inspection_has_entry_and_depth_limits_and_reads_no_file_contents() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::scratch::Scratch::new("credentials", "bounded");
        let file = dir.join("key");
        std::fs::write(&file, "SYNTHETIC_SECRET").expect("secret");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))
            .expect("unreadable file");
        let roots = vec![dir.to_path_buf()];
        assert!(expand(&roots).expect("metadata-only inspection").is_empty());
        let mut scan = Scan {
            roots: &roots,
            aliases: BTreeSet::new(),
            visited: BTreeSet::new(),
            remaining: 1,
        };
        assert!(
            scan.visit(&dir, 0)
                .expect_err("entry limit")
                .to_string()
                .contains("limit")
        );
        scan.remaining = MAX_ENTRIES;
        assert!(
            scan.visit(&file, MAX_DEPTH + 1)
                .expect_err("depth limit")
                .to_string()
                .contains("limit")
        );
    }

    #[test]
    fn directory_cycles_terminate_and_external_symlink_targets_are_excluded() {
        let dir = crate::scratch::Scratch::new("credentials", "aliases");
        let root = dir.join("store");
        std::fs::create_dir(&root).expect("store");
        std::fs::write(dir.join("external"), "SYNTHETIC_SECRET").expect("secret");
        std::os::unix::fs::symlink(&root, root.join("back")).expect("cycle");
        std::os::unix::fs::symlink(dir.join("external"), root.join("key")).expect("alias");
        assert_eq!(
            expand(&[root]).expect("finite metadata walk"),
            vec![dir.join("external")]
        );
    }
}
