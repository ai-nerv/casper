//! `sese` — asking a repository a question in words rather than in a pattern.
//!
//! `grep` answers "where does this string appear". This answers "where is session expiry handled",
//! which is a different question, and the only thing that can answer it is something that reads
//! for meaning. So the shape is: cut the repository into passages, shortlist the ones worth
//! asking about ([`scoring`]), put one question to a model through the harness ([`asking`]), and
//! show what came back with its file and line range.
//!
//! **casper holds no credential and knows no provider.** The model is reached the way rows on a
//! screen are reached: the call stops, says what it needs, and the harness answers it and runs the
//! call again. A harness with no model refuses, and the search falls back to what it worked out
//! itself and says which of the two it did.
//!
//! **A passage is named by where it is, not by its place in a list.** `src/a.rs:120-158` is what
//! the model is shown and what it answers with, so the second half of the call reads eight files
//! instead of walking the repository again to find out what `[3]` meant.
//!
//! Nothing here is cached. jevgrep, which this is modelled on, caches scores between runs;
//! `scripts/gate-writes.sh` says casper's own process writes one manifest and one log, and a cache
//! is neither.

use crate::paint::{Line, Role, Span};

pub mod asking;
pub mod eligible;
pub mod fragment;
pub mod scoring;

use eligible::Left;
use fragment::Fragment;

/// How many passages one question carries.
const SHORTLIST: usize = 48;

/// How many of them may come from one file.
const OF_EACH_FILE: usize = 4;

/// Under this, a passage is not an answer to anything.
const WORTH_SHOWING: f64 = 3.0;

/// Most files one read carries, and most bytes, both held under what an exec hands back.
const PER_READ: usize = 60;
const BYTES_PER_READ: u64 = 128 * 1024;

/// The separator between files in one read. A record separator, which source does not contain.
const BETWEEN: &str = "\u{1e}";

/// What a search is asking for.
pub struct Asked<'a> {
    pub query: &'a str,
    pub root: &'a str,
    pub limit: usize,
    /// What the harness answered, when this call is the resumption of one that asked.
    pub answered: Option<&'a str>,
}

/// What a search produced: a result, or the question it needs answered to finish one.
pub enum Sought {
    Found(Found),
    /// The arguments of a `helper` wonder, and what to say while it is being answered.
    Ask(serde_json::Value, String),
    Failed(String),
}

/// A finished search.
pub struct Found {
    pub said: String,
    pub lines: Vec<Line>,
    pub brief: String,
}

/// Run one search, or ask for what it needs to finish one.
#[must_use]
pub fn search(asked: &Asked<'_>) -> Sought {
    if asked.query.trim().is_empty() {
        return Sought::Failed("a search needs a question".to_owned());
    }
    match asked.answered {
        Some(answered) => Sought::Found(ranked(asked, answered)),
        None => looking(asked),
    }
}

/// The first half: read the repository, shortlist, and ask.
fn looking(asked: &Asked<'_>) -> Sought {
    let walked = match crate::listing::walk(asked.root, 32, false, true) {
        Ok(walked) => walked,
        Err(why) => return Sought::Failed(why),
    };
    let (candidates, mut left) = winnowed(&walked.entries, asked.root);
    let (fragments, read) = fragments(asked.root, &candidates, &mut left);
    if fragments.is_empty() {
        return Sought::Failed(format!("{}: nothing here is searchable text", asked.root));
    }

    let chosen = scoring::shortlist(asked.query, &fragments, SHORTLIST, OF_EACH_FILE);
    let shortlist: Vec<&Fragment> = chosen.iter().map(|nth| &fragments[*nth]).collect();
    if shortlist.is_empty() {
        return Sought::Found(unscored(
            asked,
            &[],
            read,
            &left,
            "nothing shared a word with it",
        ));
    }
    Sought::Ask(
        asking::question(asked.query, &shortlist),
        format!("reading {} passages of {read} files", shortlist.len()),
    )
}

/// The files worth opening, and why the others were not.
fn winnowed(entries: &[crate::listing::Entry], root: &str) -> (Vec<(String, u64)>, Vec<Left>) {
    let (mut kept, mut left) = (Vec::new(), Vec::new());
    for entry in entries.iter().filter(|entry| !entry.dir) {
        match eligible::by_name(&entry.path, entry.bytes) {
            Some(why) => left.push(why),
            None => kept.push((below(root, &entry.path), entry.bytes)),
        }
    }
    (kept, left)
}

fn below(root: &str, path: &str) -> String {
    format!("{}/{path}", root.trim_end_matches('/'))
}

/// Read the candidates and cut them up. Reading is jailed and bounded, so it happens in groups
/// small enough that what comes back has not been truncated out from under the line numbers.
fn fragments(
    root: &str,
    candidates: &[(String, u64)],
    left: &mut Vec<Left>,
) -> (Vec<Fragment>, usize) {
    let (mut out, mut read) = (Vec::new(), 0);
    for group in grouped(candidates) {
        for (path, text) in cat(&group) {
            if let Some(why) = eligible::by_content(&text) {
                left.push(why);
                continue;
            }
            read += 1;
            let shown = path
                .strip_prefix(&format!("{}/", root.trim_end_matches('/')))
                .unwrap_or(&path)
                .to_owned();
            out.extend(fragment::cut(&shown, &text));
        }
    }
    (out, read)
}

/// Candidates in groups one read can carry whole.
fn grouped(candidates: &[(String, u64)]) -> Vec<Vec<String>> {
    let (mut out, mut held, mut bytes) = (Vec::new(), Vec::new(), 0);
    for (path, size) in candidates {
        if !held.is_empty() && (held.len() >= PER_READ || bytes + size > BYTES_PER_READ) {
            out.push(std::mem::take(&mut held));
            bytes = 0;
        }
        held.push(path.clone());
        bytes += size;
    }
    if !held.is_empty() {
        out.push(held);
    }
    out
}

/// Read a group inside the jail, as one command: casper's own process stands outside the walls a
/// coordinator set, so a search can only read what a command could have read.
fn cat(group: &[String]) -> Vec<(String, String)> {
    if group.is_empty() {
        return Vec::new();
    }
    let mut argv = vec![
        "-c".to_owned(),
        format!("for f do printf '{BETWEEN}%s\\n' \"$f\"; cat -- \"$f\" || true; done"),
        "sh".to_owned(),
    ];
    argv.extend(group.iter().cloned());
    let done = crate::running::run("sh", &argv);
    done.out
        .split(BETWEEN)
        .skip(1)
        .filter_map(|block| {
            let (path, text) = block.split_once('\n')?;
            Some((path.to_owned(), text.to_owned()))
        })
        .collect()
}

/// The second half: what the model said, turned back into passages. Only the files it named are
/// read, which is why the label it was given is a place rather than a number.
fn ranked(asked: &Asked<'_>, answered: &str) -> Found {
    let (said, why) = match asking::answered(answered) {
        asking::Answer::Said(said) => (said, String::new()),
        asking::Answer::Refused(because) => (String::new(), because),
    };
    let mut scores = asking::scores(&said);
    scores.retain(|scored| scored.score >= WORTH_SHOWING);
    scores.sort_by(|a, b| b.score.total_cmp(&a.score));
    scores.truncate(asked.limit.clamp(1, 40));
    if scores.is_empty() {
        let because = if why.is_empty() {
            "nothing it was shown answers the question".to_owned()
        } else {
            why
        };
        return unscored(asked, &[], 0, &[], &because);
    }

    let paths: Vec<String> = {
        let mut paths: Vec<String> = scores
            .iter()
            .map(|scored| below(asked.root, &scored.path))
            .collect();
        paths.dedup();
        paths
    };
    let read: std::collections::HashMap<String, String> = cat(&paths).into_iter().collect();

    let (mut said, mut lines) = (String::new(), Vec::new());
    let mut shown = 0;
    for scored in &scores {
        let Some(text) = read.get(&below(asked.root, &scored.path)) else {
            continue;
        };
        let excerpt = fragment::excerpt(text, scored.from, scored.to);
        if excerpt.trim().is_empty() {
            continue;
        }
        shown += 1;
        let at = format!("{}:{}-{}", scored.path, scored.from, scored.to);
        said.push_str(&format!("{at}\n{excerpt}\n\n"));
        lines.push(vec![
            Span::new(Role::Path, at),
            Span::new(Role::Dim, format!("  {:.0}/10", scored.score)),
        ]);
    }
    let counted =
        format!("({shown} passages, ranked by meaning — read the ones that scored highest)\n");
    said.push_str(&counted);
    lines.push(vec![Span::new(Role::Dim, counted.trim_end())]);
    Found {
        said,
        lines,
        brief: format!("sese {:?} — {}", asked.query, counted.trim()),
    }
}

/// A search no model ranked, said as such. A lexical answer presented as a semantic one would be
/// worse than either of them on its own.
fn unscored(
    asked: &Asked<'_>,
    shortlist: &[&Fragment],
    read: usize,
    left: &[Left],
    because: &str,
) -> Found {
    let shown: Vec<&&Fragment> = shortlist.iter().take(asked.limit.clamp(1, 40)).collect();
    let (mut said, mut lines) = (String::new(), Vec::new());
    for fragment in &shown {
        let at = fragment.at();
        said.push_str(&format!("{at}\n{}\n\n", fragment.text));
        lines.push(vec![
            Span::new(Role::Path, at),
            Span::new(Role::Dim, "  by words"),
        ]);
    }
    let counted = counting(shown.len(), read, left, because);
    said.push_str(&counted);
    for row in counted.lines() {
        lines.push(vec![Span::new(Role::Dim, row)]);
    }
    Found {
        said,
        lines,
        brief: format!("sese {:?} — nothing ranked it: {because}", asked.query),
    }
}

/// The closing line: what was searched, what was shown, and what was not opened.
fn counting(shown: usize, read: usize, left: &[Left], because: &str) -> String {
    let mut said = format!("({shown} passages of {read} files searched");
    for why in [
        Left::Big,
        Left::Binary,
        Left::Credential,
        Left::Generated,
        Left::Locked,
        Left::Minified,
    ] {
        let count = left.iter().filter(|held| **held == why).count();
        if count > 0 {
            said.push_str(&format!("; {count} {}", why.as_str()));
        }
    }
    said.push_str(")\n");
    said.push_str(&format!("(not ranked by meaning: {because})\n"));
    said
}

#[cfg(test)]
#[path = "seeking/tests.rs"]
mod tests;
