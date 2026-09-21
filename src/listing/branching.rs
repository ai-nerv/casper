//! Drawing a walk as a tree.
//!
//! **Straight lines, no corners.** The usual `└` says "last child" by turning a corner, and the
//! same shape has to be drawn nowhere else on the line or the run stops reading as a run. Every
//! branch here is `├─`, and a branch with nothing after it simply has nothing drawn under it —
//! which says the same thing with one glyph instead of two.

use super::{Entry, Shown, Walked, sized, tallied};
use crate::paint::{Role, Span};

/// The rungs of a branch, and the column that carries on below one.
const BRANCH: &str = "├─ ";
const CARRIES: &str = "│  ";
const CLEAR: &str = "   ";

/// Most rows a tree draws before it stops and says how many it did not.
pub const MOST: usize = 400;

/// Draw `walked` as a tree rooted at `root`.
#[must_use]
pub fn tree(root: &str, walked: &Walked) -> Shown {
    let entries = &walked.entries;
    let mut rows: Vec<(String, String, String, bool)> = Vec::new();
    let mut carries: Vec<bool> = Vec::new();
    for (nth, entry) in entries.iter().take(MOST).enumerate() {
        let depth = entry.depth();
        carries.truncate(depth.saturating_sub(1));
        while carries.len() + 1 < depth {
            carries.push(false);
        }
        let name = if entry.dir {
            format!("{}/", entry.name())
        } else {
            entry.name().to_owned()
        };
        let size = if entry.dir {
            String::new()
        } else {
            sized(entry.bytes)
        };
        rows.push((
            format!("{}{BRANCH}", drawn(&carries)),
            name,
            size,
            entry.dir,
        ));
        carries.push(!last(entries, nth));
    }

    // One column for every size, found once the branches are drawn: a tree's rows are different
    // lengths, and a size padded against the longest name alone lands in a ragged column.
    let column = rows
        .iter()
        .filter(|(_, _, size, _)| !size.is_empty())
        .map(|(rungs, name, _, _)| rungs.chars().count() + name.chars().count())
        .max()
        .unwrap_or(0);

    let mut said = format!("{root}\n");
    let mut lines = vec![vec![Span::new(Role::Title, root)]];
    for (rungs, name, size, dir) in &rows {
        let pad = if size.is_empty() {
            String::new()
        } else {
            let drawn = rungs.chars().count() + name.chars().count();
            format!(
                "{:width$}  {size}",
                "",
                width = column.saturating_sub(drawn)
            )
        };
        said.push_str(&format!("{rungs}{name}{pad}\n"));
        lines.push(vec![
            Span::new(Role::Dim, rungs.as_str()),
            Span::new(if *dir { Role::Path } else { Role::Text }, name.as_str()),
            Span::new(Role::Dim, pad),
        ]);
    }

    let mut counted = tallied(entries, &walked.pruned);
    if entries.len() > MOST {
        counted = format!("{}({} more not shown)\n", counted, entries.len() - MOST);
    }
    said.push_str(&counted);
    for row in counted.lines() {
        lines.push(vec![Span::new(Role::Dim, row)]);
    }
    Shown {
        said,
        lines,
        brief: format!(
            "tree {root} ({})",
            tallied(entries, &walked.pruned)
                .trim()
                .trim_matches(['(', ')'])
        ),
    }
}

/// The columns to the left of a branch: a line under an ancestor that has more children, blank
/// under one that does not.
fn drawn(carries: &[bool]) -> String {
    carries
        .iter()
        .map(|on| if *on { CARRIES } else { CLEAR })
        .collect()
}

/// Whether the entry at `nth` is the last child of its parent. The listing is a pre-order walk, so
/// the next row at the same depth or shallower is either a sibling or an ancestor's sibling.
fn last(entries: &[Entry], nth: usize) -> bool {
    let depth = entries[nth].depth();
    entries[nth + 1..]
        .iter()
        .find(|next| next.depth() <= depth)
        .is_none_or(|next| next.depth() < depth)
}
