//! What a source file declares, for `code`: maki's index tool, its Lua walking a tree-sitter tree in
//! a VM of its own.
//!
//! The model never reaches this in casper's own process. The `code` tool runs `casper code PATH`
//! through [`crate::lua::exec`], so the file is read and parsed inside the jail, by a process that
//! sees only what a command could. The tree comes from the language's program, which
//! `casper grammar` built; see [`crate::grammars`]. The Lua is maki's, embedded: see
//! `THIRD_PARTY.md`.

mod tree;
mod treesitter;

use luna::{Closure, Executor, Lua, Value};
use std::io::Read as _;
use std::time::Instant;

/// The largest file indexed: maki's own default.
pub const MOST: u64 = 2 * 1024 * 1024;

/// The most a skeleton carries once escaped, so a reply fits through one jailed read.
const SAID: usize = 192 * 1024;

/// The most instructions one call into the indexer may run.
const BUDGET: u64 = 1_000_000_000;

/// The embedded sources, by path under `indexing/lua/`; `require` names them with dots.
macro_rules! embedded {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("indexing/lua/", $path, ".lua")))),*]
    };
}

const MODULES: &[(&str, &str)] = embedded![
    "code",
    "indexer",
    "lang/bash",
    "lang/c",
    "lang/c_sharp",
    "lang/clojure",
    "lang/containerfile",
    "lang/cpp",
    "lang/css",
    "lang/dart",
    "lang/elixir",
    "lang/gleam",
    "lang/go",
    "lang/hcl",
    "lang/html",
    "lang/java",
    "lang/json",
    "lang/kotlin",
    "lang/lua_lang",
    "lang/make",
    "lang/markdown",
    "lang/nix",
    "lang/php",
    "lang/python",
    "lang/ruby",
    "lang/rust",
    "lang/scala",
    "lang/sql",
    "lang/swift",
    "lang/toml",
    "lang/typescript",
    "lang/v",
    "lang/yaml",
    "lang/zig",
    "lang/nix/analyze",
    "lang/nix/ast",
    "lang/bazel/analyze",
    "lang/bazel/ast",
    "lang/bazel/build",
    "lang/bazel/bzl",
    "lang/bazel/module",
    "lang/bazel/render",
    "lang/bazel/util",
];

/// `require` over the embedded sources and nothing else, each run once.
const REQUIRE: &str = r#"
local sources, loaded = __modules, {}
__modules = nil
function require(name)
  local held = loaded[name]
  if held ~= nil then return held end
  local source = sources[name]
  if not source then error("module '" .. tostring(name) .. "' not found", 2) end
  local chunk, why = load(source, "=" .. name)
  if not chunk then error(why, 0) end
  held = chunk(name)
  if held == nil then held = true end
  loaded[name] = held
  return held
end
"#;

/// What `casper code` found at a path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Indexed {
    /// A source file's skeleton, and how many lines it runs to.
    File {
        path: String,
        language: String,
        skeleton: String,
        lines: usize,
    },
    /// A directory's entries, directories first, as maki lists one.
    Directory {
        path: String,
        listing: String,
        entries: usize,
    },
}

/// Index what is at `path`: a directory's entries, or a source file's skeleton.
///
/// # Errors
/// What the model is told instead, in maki's words: a missing path, an unsupported file type, a
/// file over [`MOST`], or a source the indexer could not walk.
pub fn path(path: &str) -> Result<Indexed, String> {
    let meta = std::fs::metadata(path).map_err(|_| format!("error: path not found: {path}"))?;
    if meta.is_dir() {
        return listed(path);
    }
    let mut indexer = Indexer::new()?;
    let language = indexer.language(path)?;
    if meta.len() > MOST {
        return Err(format!(
            "error: File too large ({} bytes, max {MOST}). Use read with offset/limit instead.",
            meta.len()
        ));
    }
    if !meta.is_file() {
        return Err(format!("error: not a regular file: {path}"));
    }
    let source = read(path)?;
    let skeleton = indexer.index(&source, &language)?;
    let lines = skeleton.lines().count();
    Ok(Indexed::File {
        path: path.to_owned(),
        language,
        skeleton: fitted(skeleton),
        lines,
    })
}

/// The file, refused past [`MOST`] however large it grew since it was looked at.
fn read(path: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|why| format!("error: {why}"))?;
    let mut source = Vec::new();
    file.take(MOST + 1)
        .read_to_end(&mut source)
        .map_err(|why| format!("error: {why}"))?;
    if source.len() as u64 > MOST {
        return Err(format!(
            "error: File too large (more than {MOST} bytes). Use read with offset/limit instead."
        ));
    }
    Ok(source)
}

/// A directory as maki's `dir_listing` gives it: directories with a slash, then files, each sorted.
fn listed(path: &str) -> Result<Indexed, String> {
    let entries = std::fs::read_dir(path).map_err(|why| format!("error: {why}"))?;
    let (mut names, mut files) = (Vec::new(), Vec::new());
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let dir = match entry.file_type() {
            Ok(kind) if kind.is_symlink() => {
                std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir())
            }
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        if dir {
            names.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    names.sort();
    files.sort();
    names.extend(files);
    Ok(Indexed::Directory {
        path: path.to_owned(),
        listing: names.join("\n"),
        entries: names.len(),
    })
}

/// `text`, cut at a line where its escaped length would pass [`SAID`], saying how much went.
fn fitted(text: String) -> String {
    let (mut kept, mut size) = (0, 0);
    for line in text.split_inclusive('\n') {
        size += serde_json::to_string(line).map_or(line.len() * 6, |quoted| quoted.len() - 2);
        if size > SAID {
            let head = text.get(..kept).unwrap_or_default().trim_end_matches('\n');
            return format!("{head}\n… {} more bytes, not shown", text.len() - kept);
        }
        kept += line.len();
    }
    text
}

/// The indexer's VM: casper's guarded VM, `treesitter`, and `require` over the embedded Lua.
pub struct Indexer {
    lua: Lua,
}

impl Indexer {
    /// A VM ready to index.
    ///
    /// # Errors
    /// When the embedded Lua will not load, which is a build that should not have shipped.
    pub fn new() -> Result<Self, String> {
        let until = Instant::now() + crate::running::READING;
        Self::with(&[], until, crate::shelf::Shelf::from_env())
    }

    /// A VM holding `more` beside the embedded modules, under the same `require`, whose tree
    /// calls refuse once `until` has passed and whose parsers are found on `shelf`.
    fn with(
        more: &[(&str, &str)],
        until: Instant,
        shelf: crate::shelf::Shelf,
    ) -> Result<Self, String> {
        let mut lua = crate::lua::engine::guarded();
        lua.enter(|ctx| {
            treesitter::install(ctx, until, shelf);
            let modules = luna::Table::new(&ctx);
            for (name, source) in MODULES.iter().chain(more) {
                let source = luna::String::from_slice(&ctx, source.as_bytes());
                modules.set(ctx, name.replace('/', "."), source).ok();
            }
            ctx.set_global("__modules", modules);
        });
        let mut indexer = Self { lua };
        indexer.run(REQUIRE, "require")?;
        Ok(indexer)
    }

    /// The language `path` is indexed as, or maki's message for a file type it does not read.
    ///
    /// # Errors
    /// That message, or why the indexer could not answer.
    pub fn language(&mut self, path: &str) -> Result<String, String> {
        self.set("__code_in", path.as_bytes());
        self.run(
            "__code_out, __code_why = require('code').language(__code_in)",
            "language",
        )?;
        self.answer()
    }

    /// The skeleton of `source`, read as `language`.
    ///
    /// # Errors
    /// Why the indexer could not walk it.
    pub fn index(&mut self, source: &[u8], language: &str) -> Result<String, String> {
        self.set("__code_in", source);
        self.set("__code_lang", language.as_bytes());
        self.run(
            "__code_out, __code_why = require('code').index(__code_in, __code_lang)",
            "index",
        )?;
        self.answer()
    }

    /// What the last call left: its answer, or why there was none.
    fn answer(&mut self) -> Result<String, String> {
        let (out, why) = (self.text("__code_out"), self.text("__code_why"));
        out.ok_or_else(|| why.unwrap_or_else(|| "the indexer answered nothing".to_owned()))
    }

    fn set(&mut self, name: &'static str, bytes: &[u8]) {
        self.lua.enter(|ctx| {
            ctx.set_global(name, luna::String::from_slice(&ctx, bytes));
        });
    }

    fn text(&mut self, name: &'static str) -> Option<String> {
        self.lua.enter(|ctx| match ctx.get_global_value(name) {
            Value::String(text) => Some(String::from_utf8_lossy(text.as_bytes()).into_owned()),
            _ => None,
        })
    }

    /// Run one chunk inside the guards every casper VM runs under.
    fn run(&mut self, source: &str, file: &str) -> Result<(), String> {
        if crate::lua::sandbox::precompiled(source.as_bytes()) {
            return Err(crate::lua::sandbox::PRECOMPILED.to_owned());
        }
        let executor = self
            .lua
            .try_enter(|ctx| {
                let closure = Closure::load(ctx, Some(file), source.as_bytes())?;
                Ok(ctx.stash(Executor::start(ctx, closure.into(), ())))
            })
            .map_err(|why| format!("{file}: {why}"))?;
        crate::lua::metering::metered(&mut self.lua, &executor, BUDGET)
            .map_err(|why| format!("{file}: {why}"))?;
        self.lua
            .try_enter(|ctx| ctx.fetch(&executor).take_result::<()>(ctx)?)
            .map_err(|why| format!("{file}: {why}"))
    }
}

#[cfg(test)]
#[path = "indexing/tests.rs"]
mod tests;
