//! `casper grammar fetch`: each pinned revision, and the runtime's, cloned under `src/` with git.
//!
//! One commit at depth one, checked out detached, then read back: a checkout whose `HEAD` is not
//! the pinned revision is a failure, never a grammar. Hooks are off for every command, so a
//! checkout cannot run anything of its own.

use super::{Pin, RUNTIME, progress, ran, reported};
use crate::wire::Reply;
use serde_json::json;
use std::path::Path;

/// Fetch the runtime and every chosen grammar under `root`, a grammar shared by two languages once.
pub(super) fn fetch(root: &Path, chosen: &[(&str, &Pin)]) -> Reply {
    let mut rows = Vec::new();
    let runtime = &super::lock().runtime;
    let mut pins: Vec<(&str, &Pin)> = vec![(RUNTIME, runtime)];
    pins.extend(chosen.iter().copied());
    let mut done: Vec<&str> = Vec::new();
    for (language, pin) in pins {
        let (grammar, rev) = (&pin.grammar, &pin.rev);
        let outcome = if done.contains(&grammar.as_str()) || pin.fetched(root) {
            Ok("already fetched")
        } else {
            cloned(&pin.checkout(root), pin).map(|()| "fetched")
        };
        rows.push(match outcome {
            Ok(state) => {
                done.push(grammar);
                progress(&format!("{state} {language}"));
                json!({"language": language, "grammar": grammar, "rev": rev, "state": state})
            }
            Err(why) => {
                progress(&format!("failed {language}: {why}"));
                json!({"language": language, "grammar": grammar, "rev": rev, "state": "failed",
                       "error": why})
            }
        });
    }
    reported(rows)
}

/// `pin`'s revision, alone, checked out at `checkout`.
fn cloned(checkout: &Path, pin: &Pin) -> Result<(), String> {
    if !pin.git.starts_with("https://") || !hex(&pin.rev) {
        return Err(format!(
            "{}: the lock pins {} at {}",
            pin.grammar, pin.git, pin.rev
        ));
    }
    let at = checkout.display().to_string();
    let git = |args: &[&str]| {
        let mut all = vec!["-C", at.as_str(), "-c", "core.hooksPath=/dev/null"];
        all.extend(args);
        let all: Vec<String> = all.into_iter().map(str::to_owned).collect();
        ran("git", &all, "fetch")
    };
    ran(
        "git",
        &["init".to_owned(), "-q".to_owned(), at.clone()],
        "fetch",
    )?;
    git(&[
        "fetch",
        "-q",
        "--depth",
        "1",
        "--no-tags",
        &pin.git,
        &pin.rev,
    ])?;
    git(&["checkout", "-q", "--force", "--detach", "FETCH_HEAD"])?;
    let head = git(&["rev-parse", "HEAD"])?;
    if head.trim() != pin.rev {
        return Err(format!(
            "{} checked out {} and not {}",
            pin.git,
            head.trim(),
            pin.rev
        ));
    }
    Ok(())
}

/// A full commit hash.
fn hex(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}
