//! Which passages are worth asking a model about.
//!
//! **A model cannot read a repository.** Sending every fragment of a large one would not fit, so
//! something has to choose what goes, and it is deliberately dumb: term overlap, weighted by how
//! rare each term is here and by whether it is in the path as well as the text. It is a shortlist,
//! not a ranking — the ranking is the model's, and this only decides what it gets to see.

use super::fragment::Fragment;
use std::collections::{HashMap, HashSet};

/// Words too common to tell two passages apart.
const EVERYWHERE: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "how", "i", "if",
    "in", "is", "it", "of", "on", "or", "that", "the", "then", "there", "this", "to", "we", "what",
    "when", "where", "which", "who", "why", "with", "you",
];

/// A path term counts for this many text terms: a question about session expiry is answered by
/// `session.rs` more often than by the twentieth mention of the word inside it.
const IN_PATH: f64 = 2.5;

/// The terms of a phrase: lowercased, split on anything that is not a letter or a digit, and split
/// again where a name runs two words together.
#[must_use]
pub fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        for part in humps(word) {
            if part != lower {
                out.push(part);
            }
        }
        out.push(lower);
    }
    out.retain(|term| term.len() > 1 && !EVERYWHERE.contains(&term.as_str()));
    out
}

/// `sessionExpiry` as `session` and `expiry`. A word already in one case comes back as itself.
fn humps(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut held = String::new();
    for c in word.chars() {
        if c.is_uppercase() && !held.is_empty() {
            out.push(std::mem::take(&mut held));
        }
        held.push(c.to_ascii_lowercase());
    }
    if !held.is_empty() {
        out.push(held);
    }
    out
}

/// How rare each term is across `fragments`, so a word in every file counts for little.
#[must_use]
pub fn rarity(fragments: &[Fragment]) -> HashMap<String, f64> {
    let total = fragments.len().max(1) as f64;
    let mut seen: HashMap<String, usize> = HashMap::new();
    for fragment in fragments {
        for term in terms(&fragment.text).into_iter().collect::<HashSet<_>>() {
            *seen.entry(term).or_default() += 1;
        }
    }
    seen.into_iter()
        .map(|(term, count)| (term, (total / count as f64).ln().max(0.0) + 1.0))
        .collect()
}

/// What `asked` is worth against one fragment.
#[must_use]
pub fn against(asked: &[String], fragment: &Fragment, rarity: &HashMap<String, f64>) -> f64 {
    let inside: HashSet<String> = terms(&fragment.text).into_iter().collect();
    let named: HashSet<String> = terms(&fragment.path).into_iter().collect();
    asked
        .iter()
        .map(|term| {
            let weight = rarity.get(term).copied().unwrap_or(1.0);
            let found = f64::from(u8::from(inside.contains(term)));
            let titled = if named.contains(term) { IN_PATH } else { 0.0 };
            weight * (found + titled)
        })
        .sum()
}

/// The best `most` fragments for `query`, never more than `of_each` from one file, best first. The
/// per-file cap stops one long matching file from hiding the file that answers the question.
#[must_use]
pub fn shortlist(query: &str, fragments: &[Fragment], most: usize, of_each: usize) -> Vec<usize> {
    let asked = terms(query);
    let rarity = rarity(fragments);
    let mut scored: Vec<(usize, f64)> = fragments
        .iter()
        .enumerate()
        .map(|(nth, fragment)| (nth, against(&asked, fragment, &rarity)))
        .filter(|(_, score)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut taken: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for (nth, _) in scored {
        let held = taken.entry(fragments[nth].path.as_str()).or_default();
        if *held >= of_each {
            continue;
        }
        *held += 1;
        out.push(nth);
        if out.len() >= most {
            break;
        }
    }
    out
}
