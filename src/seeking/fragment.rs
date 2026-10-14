//! Cutting a file into passages small enough to answer with. The cut prefers a blank line — the
//! boundary every language already agrees on — and falls back to a hard limit so one long block
//! cannot swallow the file.

/// One passage of one file, covering lines `from` to `to`, from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub path: String,
    pub from: usize,
    pub to: usize,
    pub text: String,
}

impl Fragment {
    /// How it is named to a model and back: a path and a line range, so an answer needs nothing
    /// kept between the question and the reply.
    #[must_use]
    pub fn at(&self) -> String {
        format!("{}:{}-{}", self.path, self.from, self.to)
    }
}

/// Most lines one fragment covers, and the fewest before a blank line is worth cutting at.
pub const LINES: usize = 40;
const SETTLED: usize = 16;

/// Cut `text` into fragments. Runs of blank lines between passages are kept with the passage above,
/// so a line number always means the line the file has there.
#[must_use]
pub fn cut(path: &str, text: &str) -> Vec<Fragment> {
    let lines: Vec<&str> = text.lines().collect();
    let mut rest = lines.as_slice();
    let mut out = Vec::new();
    let mut at = 0;
    while !rest.is_empty() {
        let end = LINES.min(rest.len());
        let cut = if end == rest.len() {
            end
        } else {
            breaking(rest, end)
        };
        let (taken, after) = rest.split_at_checked(cut).unwrap_or((rest, &[]));
        let body = taken.join("\n");
        if !body.trim().is_empty() {
            out.push(Fragment {
                path: path.to_owned(),
                from: at + 1,
                to: at + taken.len(),
                text: body,
            });
        }
        at += taken.len();
        rest = after;
    }
    out
}

/// Where to cut `lines`: the last blank line past the point a fragment stands on its own, or
/// `end`.
fn breaking(lines: &[&str], end: usize) -> usize {
    (SETTLED..end)
        .rev()
        .find(|nth| lines.get(*nth).is_some_and(|line| line.trim().is_empty()))
        .map_or(end, |nth| nth + 1)
}

/// Read a fragment's own lines back out of a file, for the excerpt a result shows.
#[must_use]
pub fn excerpt(text: &str, from: usize, to: usize) -> String {
    text.lines()
        .skip(from.saturating_sub(1))
        .take(to.saturating_sub(from) + 1)
        .collect::<Vec<&str>>()
        .join("\n")
}
