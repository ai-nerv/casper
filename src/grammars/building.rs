//! `casper grammar build`: each fetched grammar compiled with the tree-sitter runtime and casper's
//! driver into one program, `bin/<language>`, linked static where this machine can.
//!
//! `$CC` or `cc` compiles the C, `$CXX` or `c++` a C++ scanner. Objects go to `build/`, the driver's
//! source beside them, and a program replaces the one before it only once it has linked.

use super::{Pin, progress, ran, reported};
use crate::wire::Reply;
use serde_json::json;
use std::path::{Path, PathBuf};

/// The program every grammar is linked into: see its head for what it writes.
const DRIVER: &str = include_str!("driver.c");

/// A compiler named by an environment variable, as a program and the arguments it carries.
struct Compiler {
    program: String,
    args: Vec<String>,
}

impl Compiler {
    fn named(var: &str, fallback: &str) -> Self {
        let named = std::env::var(var).unwrap_or_default();
        let mut words = named.split_whitespace().map(str::to_owned);
        Self {
            program: words.next().unwrap_or_else(|| fallback.to_owned()),
            args: words.collect(),
        }
    }

    fn run(&self, root: &Path, args: &[String]) -> Result<(), String> {
        let mut all = self.args.clone();
        all.push(format!("-ffile-prefix-map={}=.", root.display()));
        all.extend(args.iter().cloned());
        ran(&self.program, &all, "build").map(|_| ())
    }
}

/// Build the runtime once, then every chosen grammar against it, several at a time.
pub(super) fn build(root: &Path, chosen: &[(&str, &Pin)]) -> Reply {
    let (cc, cxx) = (Compiler::named("CC", "cc"), Compiler::named("CXX", "c++"));
    let runtime = match runtime(root, &cc) {
        Ok(runtime) => runtime,
        Err(why) => return Reply::failed(why),
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(2, usize::from);
    let mut rows: Vec<(usize, serde_json::Value)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.min(chosen.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut rows = Vec::new();
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((language, pin)) = chosen.get(at) else {
                            return rows;
                        };
                        let built = one(root, language, pin, &runtime, (&cc, &cxx));
                        rows.push((at, row(language, pin, built)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    rows.sort_by_key(|(at, _)| *at);
    reported(rows.into_iter().map(|(_, row)| row).collect())
}

/// What one language's build came to, said as it happens.
fn row(language: &str, pin: &Pin, built: Result<(PathBuf, &str), String>) -> serde_json::Value {
    let (grammar, rev) = (&pin.grammar, &pin.rev);
    match built {
        Ok((path, linked)) => {
            progress(&format!("built {language} ({linked})"));
            let path = path.display().to_string();
            json!({"language": language, "grammar": grammar, "rev": rev, "state": "built",
                   "linked": linked, "path": path})
        }
        Err(why) => {
            progress(&format!("failed {language}: {why}"));
            json!({"language": language, "grammar": grammar, "rev": rev, "state": "failed",
                   "error": why})
        }
    }
}

/// The runtime's object, compiled from its amalgamation.
fn runtime(root: &Path, cc: &Compiler) -> Result<PathBuf, String> {
    let pin = &super::lock().runtime;
    if !pin.fetched(root) {
        return Err(format!(
            "the tree-sitter runtime is not fetched at {}: run `casper grammar fetch`",
            pin.rev
        ));
    }
    let lib = pin.checkout(root).join("lib");
    let work = root.join("build");
    std::fs::create_dir_all(&work).map_err(|why| format!("{}: {why}", work.display()))?;
    let object = work.join("tree-sitter.o");
    cc.run(
        root,
        &[
            "-c".into(),
            "-O2".into(),
            "-w".into(),
            "-std=c11".into(),
            "-D_POSIX_C_SOURCE=200112L".into(),
            "-D_DEFAULT_SOURCE".into(),
            include(&lib.join("include")),
            include(&lib.join("src")),
            include(&lib.join("src/wasm")),
            path(&lib.join("src/lib.c")),
            "-o".into(),
            path(&object),
        ],
    )?;
    Ok(object)
}

/// One grammar's program: its parser, its scanner if it has one, the runtime and the driver.
fn one(
    root: &Path,
    language: &str,
    pin: &Pin,
    runtime: &Path,
    (cc, cxx): (&Compiler, &Compiler),
) -> Result<(PathBuf, &'static str), String> {
    if !pin.fetched(root) {
        return Err(format!(
            "not fetched: run `casper grammar fetch {language}`"
        ));
    }
    let symbol = pin
        .symbol
        .as_deref()
        .filter(|symbol| {
            symbol
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .ok_or_else(|| format!("the lock names no function for {language}"))?;
    let sources = pin.sources(root)?.join("src");
    let work = root.join("build").join(language);
    let bin = root.join("bin");
    for dir in [&work, &bin] {
        std::fs::create_dir_all(dir).map_err(|why| format!("{}: {why}", dir.display()))?;
    }
    let driver = work.join("driver.c");
    std::fs::write(&driver, DRIVER).map_err(|why| format!("{}: {why}", driver.display()))?;

    let flags = |std: &str| {
        vec![
            "-c".into(),
            "-O2".into(),
            "-w".into(),
            std.into(),
            include(&sources),
        ]
    };
    let mut objects = vec![path(runtime)];
    let mut plus = false;
    for (file, c) in [
        ("parser.c", true),
        ("scanner.c", true),
        ("scanner.cc", false),
    ] {
        let source = sources.join(file);
        if !source.is_file() {
            if file == "parser.c" {
                return Err(format!("{} has no src/parser.c", pin.grammar));
            }
            continue;
        }
        let object = work.join(file.replace('.', "_") + ".o");
        let (compiler, mut args) = if c {
            (cc, flags("-std=gnu11"))
        } else {
            plus = true;
            (cxx, flags("-std=c++14"))
        };
        args.extend([path(&source), "-o".into(), path(&object)]);
        compiler.run(root, &args)?;
        objects.push(path(&object));
    }
    let lib = super::lock().runtime.checkout(root).join("lib/include");
    let object = work.join("driver.o");
    cc.run(
        root,
        &[
            "-c".into(),
            "-O2".into(),
            "-std=c11".into(),
            "-D_POSIX_C_SOURCE=200809L".into(),
            include(&lib),
            format!("-DCASPER_LANGUAGE={symbol}"),
            format!("-DCASPER_REV=\"{}\"", pin.rev),
            path(&driver),
            "-o".into(),
            path(&object),
        ],
    )?;
    objects.push(path(&object));

    let linker = if plus { cxx } else { cc };
    let program = crate::shelf::binary(root, language);
    let part = program.with_extension("part");
    let link = |extra: &[&str]| {
        let mut args: Vec<String> = extra.iter().map(|arg| (*arg).to_owned()).collect();
        args.extend(["-s".into(), "-o".into(), path(&part)]);
        args.extend(objects.iter().cloned());
        linker.run(root, &args)
    };
    let linked = match link(&["-static"]) {
        Ok(()) => "static",
        Err(_) => link(&[]).map(|()| "dynamic")?,
    };
    std::fs::rename(&part, &program).map_err(|why| format!("{}: {why}", program.display()))?;
    Ok((program, linked))
}

fn include(dir: &Path) -> String {
    format!("-I{}", dir.display())
}

fn path(at: &Path) -> String {
    at.display().to_string()
}
