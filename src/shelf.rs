//! Where `code` finds the program it parses a language with: `bin/<language>` under each
//! directory [`NAMED`] lists, then under `$XDG_DATA_HOME/casper/grammars`, where
//! `casper grammar fetch` builds them.
//!
//! Nothing here builds or runs one. The jail reads [`Shelf::bins`] to grant those directories, the
//! indexer reads [`Shelf::find`], and `casper grammar` builds into [`home`].

use std::path::{Path, PathBuf};

/// Directories searched before the data directory, separated as `PATH` is: how the tests and the
/// jail name a set somewhere else.
pub const NAMED: &str = "CASPER_GRAMMARS";

/// The grammar directories, in the order they are searched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shelf {
    roots: Vec<PathBuf>,
}

impl Shelf {
    /// [`NAMED`]'s directories, then [`home`].
    #[must_use]
    pub fn from_env() -> Self {
        Self::then(home())
    }

    /// [`NAMED`]'s directories, then `last`.
    #[must_use]
    pub fn then(last: Option<PathBuf>) -> Self {
        let mut roots: Vec<PathBuf> = std::env::var_os(NAMED)
            .map(|named| {
                std::env::split_paths(&named)
                    .filter(|root| root.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        roots.extend(last);
        Self { roots }
    }

    /// Exactly these directories.
    #[must_use]
    pub fn at(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The program that parses `language`, from the first directory holding one. A name that could
    /// leave `bin/` is never looked up.
    #[must_use]
    pub fn find(&self, language: &str) -> Option<PathBuf> {
        if !plain(language) {
            return None;
        }
        self.roots
            .iter()
            .map(|root| root.join("bin").join(language))
            .find(|path| path.is_file())
    }

    /// Every `bin` directory there is, canonical: what a jailed `casper code` may read and run.
    #[must_use]
    pub fn bins(&self) -> Vec<PathBuf> {
        let mut bins: Vec<PathBuf> = self
            .roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root.join("bin")).ok())
            .filter(|bin| bin.is_dir())
            .collect();
        bins.dedup();
        bins
    }

    /// The directories as [`NAMED`] would carry them.
    #[must_use]
    pub fn joined(&self) -> Option<std::ffi::OsString> {
        std::env::join_paths(&self.roots).ok()
    }
}

/// `$XDG_DATA_HOME/casper/grammars`, or the same under `~/.local/share`.
#[must_use]
pub fn home() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        })?;
    Some(data.join("casper/grammars"))
}

/// A language name: letters, digits and `_`, and nothing a path could be built from.
#[must_use]
pub fn plain(language: &str) -> bool {
    !language.is_empty()
        && language
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `bin/<language>` under `root`.
#[must_use]
pub fn binary(root: &Path, language: &str) -> PathBuf {
    root.join("bin").join(language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn the_first_directory_holding_a_language_answers_for_it() {
        let (first, second) = (Scratch::new("shelf", "a"), Scratch::new("shelf", "b"));
        for root in [&first, &second] {
            std::fs::create_dir(root.join("bin")).expect("bin");
        }
        std::fs::write(second.join("bin/rust"), "").expect("rust");
        std::fs::write(second.join("bin/go"), "").expect("go");
        std::fs::write(first.join("bin/go"), "").expect("go");
        let shelf = Shelf::at(vec![first.to_path_buf(), second.to_path_buf()]);
        assert_eq!(shelf.find("rust"), Some(second.join("bin/rust")));
        assert_eq!(shelf.find("go"), Some(first.join("bin/go")));
        assert_eq!(shelf.find("zig"), None);
        assert_eq!(shelf.bins().len(), 2);
    }

    #[test]
    fn a_name_that_could_leave_bin_is_never_looked_up() {
        let root = Scratch::new("shelf", "escape");
        std::fs::create_dir(root.join("bin")).expect("bin");
        std::fs::write(root.join("secret"), "").expect("secret");
        let shelf = Shelf::at(vec![root.to_path_buf()]);
        for name in ["../secret", "", "a/b", "Rust", "c-sharp"] {
            assert_eq!(shelf.find(name), None, "{name}");
        }
        assert!(plain("c_sharp") && plain("v"));
    }
}
