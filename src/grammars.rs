//! The parsers `code` reads with, fetched and built on this machine the way Helix builds its own,
//! from the grammar list Helix pins: nothing of a grammar is compiled into casper.
//!
//! ```text
//! casper grammar fetch [lang…]   clone each pinned revision, and the tree-sitter runtime
//! casper grammar build [lang…]   compile each into bin/<lang>, a program that prints a tree
//! casper grammar list            each language: built, stale, fetched or missing
//! ```
//!
//! What is pinned is `grammars.lock`, generated from Helix's `languages.toml` by
//! `make grammars-lock` and never edited by hand. The network is reached only by `fetch`, and only
//! when somebody runs it. Built programs land under [`crate::shelf::home`], or `--dir`.

mod building;
mod fetching;

use crate::shelf::Shelf;
use crate::wire::Reply;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The lock, as generated.
const LOCK: &str = include_str!("../grammars.lock");

/// Where the runtime's sources are fetched, under `src/`.
const RUNTIME: &str = "tree-sitter";

/// One pinned source: a git repository at a revision, and where in it the grammar is.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// Helix's name for the grammar, and the directory its sources are fetched into.
    pub grammar: String,
    pub git: String,
    pub rev: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub subpath: Option<String>,
    /// The grammar's `tree_sitter_<name>` function.
    #[serde(default)]
    pub symbol: Option<String>,
    /// The fetched tree's hash, for Nix's fixed-output fetch.
    pub hash: String,
    /// Why this pin is not Helix's, when it is not.
    #[serde(default)]
    pub note: Option<String>,
}

/// Every pin: Helix's release, the runtime, and one grammar per parser the indexer asks for.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub about: String,
    pub helix: String,
    pub languages: String,
    pub runtime: Pin,
    pub grammars: BTreeMap<String, Pin>,
}

/// The lock casper was built with.
///
/// # Panics
/// Never for a lock `make grammars-lock` wrote; a test holds it to that.
#[must_use]
pub fn lock() -> &'static Lock {
    static PARSED: std::sync::OnceLock<Lock> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(LOCK).unwrap_or_else(|why| {
            crate::noted!("grammars: the lock does not parse: {why}");
            Lock {
                about: String::new(),
                helix: String::new(),
                languages: String::new(),
                runtime: Pin::empty(),
                grammars: BTreeMap::new(),
            }
        })
    })
}

/// The pin for a parser the indexer asks for by `language`.
#[must_use]
pub fn pinned(language: &str) -> Option<&'static Pin> {
    lock().grammars.get(language)
}

impl Pin {
    fn empty() -> Self {
        Self {
            grammar: String::new(),
            git: String::new(),
            rev: String::new(),
            tag: None,
            subpath: None,
            symbol: None,
            hash: String::new(),
            note: None,
        }
    }

    /// Where the repository is fetched, under `root`.
    fn checkout(&self, root: &Path) -> PathBuf {
        root.join("src").join(&self.grammar)
    }

    /// The grammar's own directory in its checkout: `src/parser.c` is beneath it.
    fn sources(&self, root: &Path) -> Result<PathBuf, String> {
        let mut dir = self.checkout(root);
        if let Some(sub) = &self.subpath {
            let inside = Path::new(sub)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)));
            if !inside {
                return Err(format!(
                    "{}: the subpath {sub} leaves its checkout",
                    self.grammar
                ));
            }
            dir.push(sub);
        }
        Ok(dir)
    }

    /// Whether the checkout under `root` is at this pin's revision.
    fn fetched(&self, root: &Path) -> bool {
        std::fs::read_to_string(self.checkout(root).join(".git/HEAD"))
            .is_ok_and(|head| head.trim() == self.rev)
    }
}

/// `casper grammar …`: `args` as the command line gave them, the verb first.
#[must_use]
pub fn verb(args: &[String]) -> Reply {
    let mut words = args
        .iter()
        .skip(1)
        .filter(|arg| *arg != "--json" && *arg != "--cbor");
    let Some(action) = words.next() else {
        return Reply::refused(USAGE);
    };
    let (mut dir, mut named) = (None, Vec::new());
    while let Some(word) = words.next() {
        if word == "--dir" {
            dir = words.next().map(PathBuf::from);
        } else if let Some(at) = word.strip_prefix("--dir=") {
            dir = Some(PathBuf::from(at));
        } else {
            named.push(word.clone());
        }
    }
    let Some(root) = dir.or_else(crate::shelf::home) else {
        return Reply::refused("no data directory: set XDG_DATA_HOME or HOME, or pass --dir");
    };
    let root = std::path::absolute(&root).unwrap_or(root);
    let chosen = match chosen(&named) {
        Ok(chosen) => chosen,
        Err(why) => return Reply::refused(why),
    };
    match action.as_str() {
        "fetch" => fetching::fetch(&root, &chosen),
        "build" => building::build(&root, &chosen),
        "list" => listed(&Shelf::then(Some(root.clone())), &root, &chosen),
        other => Reply::refused(format!("no such grammar action: {other}\n{USAGE}")),
    }
}

const USAGE: &str = "casper grammar fetch|build|list [--dir DIR] [language…]";

/// The languages `named` asks for, or every one when it names none.
fn chosen(named: &[String]) -> Result<Vec<(&'static str, &'static Pin)>, String> {
    let all = &lock().grammars;
    if named.is_empty() {
        return Ok(all.iter().map(|(lang, pin)| (lang.as_str(), pin)).collect());
    }
    let unknown: Vec<&str> = named
        .iter()
        .filter(|lang| !all.contains_key(lang.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = all.keys().map(String::as_str).collect();
        return Err(format!(
            "no grammar for {}; there is one for {}",
            unknown.join(", "),
            known.join(" ")
        ));
    }
    Ok(all
        .iter()
        .filter(|(lang, _)| named.contains(lang))
        .map(|(lang, pin)| (lang.as_str(), pin))
        .collect())
}

/// Each language as built, stale, fetched or missing: built on `shelf`, fetched under `root`.
fn listed(shelf: &Shelf, root: &Path, chosen: &[(&str, &Pin)]) -> Reply {
    let rows = chosen
        .iter()
        .map(|(lang, pin)| {
            let found = shelf.find(lang);
            let state = match &found {
                Some(path) if built_from(path).as_deref() == Some(pin.rev.as_str()) => "built",
                Some(_) => "stale",
                None if pin.fetched(root) => "fetched",
                None => "missing",
            };
            let path = found.map(|path| path.display().to_string());
            json!({"language": lang, "grammar": pin.grammar, "rev": pin.rev, "state": state,
                   "path": path})
        })
        .collect();
    Reply::rows(rows)
}

/// The revision a built program says it was built from.
fn built_from(program: &Path) -> Option<String> {
    let done = crate::running::run(&program.display().to_string(), &["--rev".to_owned()]);
    (done.code == 0).then(|| done.out.trim().to_owned())
}

/// Rows for each language's outcome, failing the reply when any failed.
fn reported(rows: Vec<serde_json::Value>) -> Reply {
    let failed: Vec<String> = rows
        .iter()
        .filter(|row| row["state"] == "failed")
        .map(|row| {
            format!(
                "{}: {}",
                row["language"].as_str().unwrap_or_default(),
                row["error"].as_str().unwrap_or_default()
            )
        })
        .collect();
    if failed.is_empty() {
        return Reply::rows(rows);
    }
    Reply {
        n: rows.len(),
        result: rows,
        ..Reply::failed(failed.join("\n"))
    }
}

/// A command's outcome, with what to do when the program is not there at all.
fn ran(program: &str, args: &[String], needed_by: &str) -> Result<String, String> {
    let done = crate::running::run(program, args);
    match done.code {
        0 => Ok(done.out),
        -1 => Err(format!(
            "{program} is not installed or not on PATH, and `casper grammar {needed_by}` needs it"
        )),
        code => {
            let said = done.err.trim();
            let said = if said.is_empty() {
                done.out.trim()
            } else {
                said
            };
            Err(format!("{program} exited {code}: {said}"))
        }
    }
}

/// One line of progress for the person waiting, on standard error.
fn progress(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "casper grammar: {line}");
}

#[cfg(test)]
#[path = "grammars/tests.rs"]
mod tests;
