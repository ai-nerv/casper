//! What a directory holds, for `ls` and `tree`.
//!
//! The walk happens inside the jail, through [`crate::running`], for the same reason `read` cats
//! a file rather than opening it: casper's own process stands outside the walls a coordinator set,
//! and a lister that read the filesystem by hand would answer for paths no command may touch.
//! Everything after the walk — what is worth showing, in what order, drawn how — is here.

use crate::paint::{Line, Role, Span};

mod branching;
pub use branching::tree;

/// One thing a directory holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the root that was walked. Never empty.
    pub path: String,
    pub dir: bool,
    pub bytes: u64,
}

impl Entry {
    /// The last component, which is what a listing shows.
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// How deep below the root it sits. A child of the root is `1`.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.path.split('/').count()
    }
}

/// Directory names never worth walking into: a build's output, a dependency tree, a repository's
/// own bookkeeping. Walked, they bury the answer under a hundred thousand files nobody asked about.
pub const BURIED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".cargo",
    ".direnv",
    ".next",
    ".nuxt",
    ".gradle",
    ".terraform",
    "Pods",
];

/// What was found, and what the walk could not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walked {
    pub entries: Vec<Entry>,
    /// Names pruned rather than descended into, so a listing can say what it did not open.
    pub pruned: Vec<String>,
}

/// Walk `root` to `depth`, pruning [`BURIED`]. `hidden` keeps dotfiles, and `tracked` holds the
/// walk to what version control accounts for where there is a repository to ask.
///
/// # Errors
/// What the walker said when it would not run: a missing directory, or a jail that refused.
pub fn walk(root: &str, depth: usize, hidden: bool, tracked: bool) -> Result<Walked, String> {
    let done = crate::running::run("find", &argv(root, depth));
    if done.code != 0 {
        let why = done.err.trim();
        return Err(if why.is_empty() {
            format!("{root} could not be listed")
        } else {
            why.to_owned()
        });
    }
    let mut walked = sifted(&done.out, hidden);
    if tracked && let Some(accounted) = Accounted::under(root) {
        accounted.restrict(&mut walked);
    }
    Ok(walked)
}

/// The paths version control accounts for under a root: the files it tracks, the ones it does not
/// yet but no ignore rule covers, and every directory holding one.
///
/// An ignored tree is where a listing goes from a page to a hundred thousand rows, and the repository
/// already says which those are.
pub struct Accounted(std::collections::HashSet<String>);

/// How far down a chain of nested repositories the accounting still follows.
const NESTED: usize = 3;

impl Accounted {
    /// Ask about `root`, and about every repository nested inside it: a submodule is one path to
    /// the repository above and a whole tree to the person reading the listing.
    ///
    /// `None` where git answered no repository, and nothing is held back.
    fn under(root: &str) -> Option<Self> {
        let mut kept = std::collections::HashSet::new();
        gathered(root, "", &mut kept, 0).then(|| Self(kept))
    }

    /// Every named path, and the directories above each one.
    #[cfg(test)]
    fn of<'a>(paths: impl Iterator<Item = &'a str>) -> Self {
        let mut kept = std::collections::HashSet::new();
        held(paths, "", &mut kept);
        Self(kept)
    }

    /// Drop what is not accounted for, counting a shut directory among the ones not opened — but
    /// only the topmost of a shut tree, or one closed door would be tallied as a dozen.
    fn restrict(&self, walked: &mut Walked) {
        let mut shut = Vec::new();
        walked.entries.retain(|entry| {
            if self.0.contains(&entry.path) {
                return true;
            }
            let topmost = entry
                .path
                .rsplit_once('/')
                .is_none_or(|(above, _)| self.0.contains(above));
            if entry.dir && topmost {
                shut.push(entry.path.clone());
            }
            false
        });
        walked.pruned.extend(shut);
        walked.pruned.sort();
        walked.pruned.dedup();
    }
}

/// The walker's arguments. `-maxdepth` leads, because find reads it as a global option wherever it
/// is written and warns when it is not first; the prune comes before the print so a buried
/// directory is named once and never descended.
fn argv(root: &str, depth: usize) -> Vec<String> {
    let mut argv = vec![
        root.to_owned(),
        "-maxdepth".to_owned(),
        depth.clamp(1, 32).to_string(),
        "-mindepth".to_owned(),
        "1".to_owned(),
        "(".to_owned(),
    ];
    for (nth, name) in BURIED.iter().enumerate() {
        if nth > 0 {
            argv.push("-o".to_owned());
        }
        argv.push("-name".to_owned());
        argv.push((*name).to_owned());
    }
    argv.extend([
        ")".to_owned(),
        "-prune".to_owned(),
        "-printf".to_owned(),
        "%y\\t%s\\t%P\\n".to_owned(),
        "-o".to_owned(),
        "-printf".to_owned(),
        "%y\\t%s\\t%P\\n".to_owned(),
    ]);
    argv
}

/// Ask one repository, then the ones nested inside it. `under` is the prefix a path there carries
/// in the listing above; `true` when git answered at all.
fn gathered(
    root: &str,
    under: &str,
    kept: &mut std::collections::HashSet<String>,
    depth: usize,
) -> bool {
    let argv = [
        "-C",
        root,
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ]
    .map(str::to_owned);
    let done = crate::running::run("git", &argv);
    if done.code != 0 {
        return false;
    }
    held(done.out.split('\0'), under, kept);
    if depth < NESTED {
        for sub in nested(root) {
            gathered(
                &format!("{root}/{sub}"),
                &format!("{under}{sub}/"),
                kept,
                depth + 1,
            );
        }
    }
    true
}

/// The submodules a repository declares, by the path each sits at.
fn nested(root: &str) -> Vec<String> {
    let argv = [
        "-C",
        root,
        "config",
        "-f",
        ".gitmodules",
        "--get-regexp",
        "^submodule\\..*\\.path$",
    ]
    .map(str::to_owned);
    let done = crate::running::run("git", &argv);
    if done.code != 0 {
        return Vec::new();
    }
    done.out
        .lines()
        .filter_map(|row| row.split_once(' '))
        .map(|(_, path)| path.trim().to_owned())
        .filter(|path| !path.is_empty() && !path.starts_with('/') && !path.contains(".."))
        .collect()
}

/// Put each path under `under`, with the directories above it.
fn held<'a>(
    paths: impl Iterator<Item = &'a str>,
    under: &str,
    kept: &mut std::collections::HashSet<String>,
) {
    for path in paths.filter(|path| !path.is_empty()) {
        let full = format!("{under}{path}");
        let mut below = full.as_str();
        while let Some((above, _)) = below.rsplit_once('/') {
            if !kept.insert(above.to_owned()) {
                break;
            }
            below = above;
        }
        kept.insert(full);
    }
}

/// Read the walker's rows, dropping what a listing should not show.
fn sifted(out: &str, hidden: bool) -> Walked {
    let (mut entries, mut pruned) = (Vec::new(), Vec::new());
    for row in out.lines() {
        let mut fields = row.splitn(3, '\t');
        let (Some(kind), Some(size), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let dir = kind == "d";
        if dir && BURIED.contains(&path.rsplit('/').next().unwrap_or(path)) {
            pruned.push(path.to_owned());
            continue;
        }
        if !hidden && path.split('/').any(|part| part.starts_with('.')) {
            continue;
        }
        entries.push(Entry {
            path: path.to_owned(),
            dir,
            bytes: size.parse().unwrap_or(0),
        });
    }
    entries.sort_by_key(ordered);
    pruned.sort();
    pruned.dedup();
    Walked { entries, pruned }
}

/// Directories first within a parent, then by name, so the shape of a place reads before its
/// contents do. Compared component by component, or `src/a` would sort between `src` and `srcs`.
fn ordered(entry: &Entry) -> Vec<(bool, String)> {
    let parts: Vec<&str> = entry.path.split('/').collect();
    parts
        .iter()
        .enumerate()
        .map(|(nth, part)| {
            let leaf = nth + 1 == parts.len();
            (leaf && !entry.dir, (*part).to_lowercase())
        })
        .collect()
}

/// A size as a person reads it: three significant figures and a unit, never more.
#[must_use]
pub fn sized(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes}{}", UNITS[0]);
    }
    if size < 10.0 {
        format!("{size:.1}{}", UNITS[unit])
    } else {
        format!("{size:.0}{}", UNITS[unit])
    }
}

/// A result with both faces filled: what the model reads, and what the person is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub said: String,
    pub lines: Vec<Line>,
    pub brief: String,
}

/// One directory, its children only.
#[must_use]
pub fn list(root: &str, walked: &Walked) -> Shown {
    let named = |entry: &Entry| {
        if entry.dir {
            format!("{}/", entry.name())
        } else {
            entry.name().to_owned()
        }
    };
    let column = walked
        .entries
        .iter()
        .filter(|entry| !entry.dir)
        .map(|entry| named(entry).chars().count())
        .max()
        .unwrap_or(0);

    let (mut said, mut lines) = (String::new(), Vec::new());
    for entry in &walked.entries {
        let name = named(entry);
        let size = if entry.dir {
            String::new()
        } else {
            let pad = column.saturating_sub(name.chars().count());
            format!("{:pad$}  {}", "", sized(entry.bytes))
        };
        said.push_str(&format!("{name}{size}\n"));
        lines.push(vec![
            Span::new(
                if entry.dir { Role::Path } else { Role::Text },
                name.as_str(),
            ),
            Span::new(Role::Dim, size),
        ]);
    }
    let counted = tallied(&walked.entries, &walked.pruned);
    said.push_str(&counted);
    lines.push(vec![Span::new(Role::Dim, counted.trim_end())]);
    Shown {
        said,
        lines,
        brief: format!("ls {root} ({})", counted.trim().trim_matches(['(', ')'])),
    }
}

/// The closing line every listing ends on: what was there, and what was left shut.
pub(crate) fn tallied(entries: &[Entry], pruned: &[String]) -> String {
    let dirs = entries.iter().filter(|entry| entry.dir).count();
    let files = entries.len() - dirs;
    let mut said = format!(
        "({files} file{}, {dirs} director{}",
        if files == 1 { "" } else { "s" },
        if dirs == 1 { "y" } else { "ies" }
    );
    if !pruned.is_empty() {
        said.push_str(&format!("; {} not opened", pruned.len()));
    }
    said.push_str(")\n");
    said
}

#[cfg(test)]
#[path = "listing/tests.rs"]
mod tests;
