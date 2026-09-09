//! Where declarations come from, and in what order.
//!
//! neovim's model: a runtimepath of roots, `plugin/` read at startup, `after/` last, and the path
//! a coordinator gave last of all. Every discovered file runs in the same sandboxed VM as the
//! shipped declarations, so dropping a file in a directory extends casper and cannot itself spawn
//! a process — see [`crate::lua::sandbox`].

use std::path::{Path, PathBuf};

/// Where a file came from, which is what decides whether it runs on sight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The owner's own, or a coordinator's. Runs on sight.
    Owner,
    /// A package installed under `site/`, which runs once it has been acknowledged.
    Installed,
}

impl Trust {
    /// Whether it has to be acknowledged before it runs.
    #[must_use]
    pub fn needs_acknowledging(self) -> bool {
        matches!(self, Self::Installed)
    }
}

/// The roots declarations are read from.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    /// `$XDG_CONFIG_HOME/casper`, the owner's own.
    pub config: Option<PathBuf>,
    /// `$XDG_DATA_HOME/casper/site`, where installed packages live.
    pub site: Option<PathBuf>,
    /// What a coordinator said, read last of all.
    pub given: Option<PathBuf>,
}

/// The config directory: `$XDG_CONFIG_HOME/casper`, or `~/.config/casper`. Named, not searched:
/// a relative lookup would load whichever checkout the working directory happened to be in.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg).join("casper"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".config/casper"))
}

/// Where installed packages live.
#[must_use]
pub fn site_dir() -> Option<PathBuf> {
    data_home().map(|home| home.join("casper/site"))
}

/// `$XDG_DATA_HOME`, or `~/.local/share`.
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })
}

/// Every file to read, in the order to read it.
///
/// ```text
///   <config>/tools.lua                 the owner's own, as it has always been
///   <config>/plugin/*.lua              alphabetical, each on its own
///   <site>/pack/*/start/*/plugin/*.lua installed packages
///   <config>/after/plugin/*.lua        the last word
///   <given>                            what a coordinator handed over
/// ```
#[must_use]
pub fn runtimepath(roots: &Roots) -> Vec<(PathBuf, Trust)> {
    let mut out = Vec::new();

    if let Some(config) = &roots.config {
        let theirs = config.join("tools.lua");
        if theirs.is_file() {
            out.push((theirs, Trust::Owner));
        }
        out.extend(
            lua_files(&config.join("plugin"))
                .into_iter()
                .map(|path| (path, Trust::Owner)),
        );
    }

    if let Some(site) = &roots.site {
        for package in packages(&site.join("pack")) {
            out.extend(
                lua_files(&package.join("plugin"))
                    .into_iter()
                    .map(|path| (path, Trust::Installed)),
            );
        }
    }

    // The registry replaces by name, so the last declaration of a name is the one that stands.
    if let Some(config) = &roots.config {
        out.extend(
            lua_files(&config.join("after/plugin"))
                .into_iter()
                .map(|path| (path, Trust::Owner)),
        );
    }

    if let Some(given) = &roots.given
        && given.is_file()
    {
        out.push((given.clone(), Trust::Owner));
    }
    out
}

/// Every `.lua` directly in a directory, sorted rather than in the order the filesystem answers:
/// a load order that changes between machines is a set of tools that behaves differently on each.
fn lua_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|end| end == "lua"))
        .collect();
    found.sort();
    found
}

/// Every installed package under `pack/*/start/*`.
fn packages(pack: &Path) -> Vec<PathBuf> {
    let Ok(groups) = std::fs::read_dir(pack) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for group in groups.flatten() {
        let Ok(entries) = std::fs::read_dir(group.path().join("start")) else {
            continue;
        };
        found.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::Scratch;

    fn touch(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, body).expect("write");
    }

    fn at(config: PathBuf) -> Roots {
        Roots {
            config: Some(config),
            site: None,
            given: None,
        }
    }

    #[test]
    fn dropping_a_file_in_plugin_is_enough_to_declare_a_tool() {
        let dir = Scratch::new("casper-rtp", "dropped");
        touch(&dir.join("plugin/mine.lua"), "-- nothing\n");

        let files = runtimepath(&at(dir.to_path_buf()));
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].0.ends_with("plugin/mine.lua"));
        assert!(
            !files[0].1.needs_acknowledging(),
            "your own file runs on sight"
        );
    }

    #[test]
    fn the_order_is_tools_then_plugin_then_pack_then_after_then_given() {
        let dir = Scratch::new("casper-rtp", "order");
        let config = dir.join("config");
        let site = dir.join("site");
        let given = dir.join("given.lua");
        touch(&config.join("tools.lua"), "");
        touch(&config.join("plugin/b.lua"), "");
        touch(&site.join("pack/vendor/start/thing/plugin/c.lua"), "");
        touch(&config.join("after/plugin/d.lua"), "");
        touch(&given, "");

        let files = runtimepath(&Roots {
            config: Some(config),
            site: Some(site),
            given: Some(given),
        });
        let names: Vec<_> = files
            .iter()
            .filter_map(|(path, _)| path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(names, ["tools.lua", "b.lua", "c.lua", "d.lua", "given.lua"]);

        let theirs: Vec<_> = files
            .iter()
            .filter(|(_, trust)| trust.needs_acknowledging())
            .filter_map(|(path, _)| path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(theirs, ["c.lua"]);
    }

    #[test]
    fn nothing_installed_is_no_files_rather_than_an_error() {
        let dir = Scratch::new("casper-rtp", "empty");
        let files = runtimepath(&Roots {
            config: Some(dir.join("nowhere")),
            site: Some(dir.join("also-nowhere")),
            given: Some(dir.join("nor-here.lua")),
        });
        assert!(files.is_empty(), "{files:?}");
    }

    #[test]
    fn only_lua_files_are_picked_up() {
        let dir = Scratch::new("casper-rtp", "kinds");
        touch(&dir.join("plugin/real.lua"), "");
        touch(&dir.join("plugin/README.md"), "");
        touch(&dir.join("plugin/real.lua.bak"), "");

        let files = runtimepath(&at(dir.to_path_buf()));
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].0.ends_with("real.lua"));
    }
}
