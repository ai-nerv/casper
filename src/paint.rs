//! What a tool's output *means*, which is what magi turns into a colour.
//!
//! A tool never chooses a colour. It names a [`Role`] — `added`, `keyword`, `path` — and the
//! harness resolves that against its own palette. That is the whole reason a `patch` and a
//! syntax-highlighted `cat` agree on screen: both emit `added` or `comment`, and one palette
//! paints them. A tool that sent a colour would be a second palette, and it would be wrong on
//! the first theme anybody set.
//!
//! **These types are casper's own, and the JSON is the contract.** magi has its own copy in
//! `magi-proto::tooling`. Siblings do not share crates — that is what makes them siblings — so
//! what is pinned is the encoding, by round-trip tests on both sides. A field renamed here and
//! not there is a role that silently becomes `text` on the far side, which is why the names are
//! tested rather than assumed.

use serde::{Deserialize, Serialize};

/// What a span of text is.
///
/// Closed on purpose. An open vocabulary is a second palette: a tool naming its own role would be
/// asking the harness to invent a colour for it, and the answer would differ per tool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Ordinary text.
    #[default]
    Text,
    /// Present but secondary.
    Muted,
    /// Present and nearly out of the way.
    Dim,
    /// A heading, or the name of the thing below it.
    Title,
    /// A path, a filename, a location.
    Path,
    /// It worked.
    Ok,
    /// It worked, and something is worth knowing.
    Warn,
    /// It did not work.
    Error,
    /// A line a patch adds.
    Added,
    /// A line a patch removes.
    Removed,
    /// The `@@` and `+++` rows, which say *where* rather than what.
    Marker,
    /// A line a patch leaves alone.
    Context,
    /// A language keyword.
    Keyword,
    /// A string literal.
    String,
    /// A numeric literal.
    Number,
    /// A comment.
    Comment,
    /// A type name.
    Type,
    /// A function name.
    Func,
}

/// A run of text with one meaning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// What this text is.
    #[serde(default)]
    pub role: Role,
    /// The text itself.
    pub text: String,
    /// A colour chosen outright, overriding the role.
    ///
    /// **The one exception to "a tool never chooses a colour".** Roles exist so a `patch` and a
    /// highlighted `cat` agree with the rest of the screen — they are output, read alongside
    /// everything else, and a tool picking its own green would be a second palette to keep in
    /// step. A surface drawing a *picture* is not that: a dinosaur is brown whatever anybody's
    /// theme says, and asking for `added` there would be a role lying about itself to get green.
    ///
    /// Surfaces may use this; tool output should not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    /// A background chosen outright, for the same narrow reason as [`Span::rgb`].
    ///
    /// What makes a run of text read as *inverted* rather than merely coloured, which is how a
    /// picture says "this is held down right now" without a second row to say it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<[u8; 3]>,
}

impl Span {
    /// A span of `text` in `role`.
    #[must_use]
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            rgb: None,
            bg: None,
        }
    }
}

/// One line, as the spans it is made of.
pub type Line = Vec<Span>;

/// Paint a unified diff.
///
/// Structure, not colour, and read from the line's first character the way every diff reader
/// does. The `+++`, `---` and `@@` rows are neither added nor removed — they say *where* — so
/// they get a role of their own, and a diff reads as three things rather than two and a lie.
#[must_use]
pub fn diff(text: &str) -> Vec<Line> {
    text.lines()
        .map(|line| {
            // The file and hunk headers first, because they start with the same characters an
            // added or removed line does and would otherwise read as a diff that adds and
            // removes its own filename.
            let heading =
                line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@");
            let role = if heading {
                Role::Marker
            } else {
                match line.as_bytes().first() {
                    Some(b'+') => Role::Added,
                    Some(b'-') => Role::Removed,
                    _ => Role::Context,
                }
            };
            vec![Span::new(role, line)]
        })
        .collect()
}

/// Paint plain text, saying nothing about it.
///
/// The floor. A tool with no adapter still produces lines a harness can draw, in one role, which
/// is legible and never wrong.
#[must_use]
pub fn plain(text: &str) -> Vec<Line> {
    text.lines()
        .map(|line| vec![Span::new(Role::Text, line)])
        .collect()
}

/// A foreground colour, as a program actually asked for it.
///
/// Both, because both are in use and a parser that knew one would silently learn nothing from
/// the other. `bat` emits truecolour by default — `38;2;171;178;191` — and a theme that only
/// spoke 256-colour indices would match none of it and paint a whole file as plain text, which
/// is exactly what it looks like when this is wrong: legible, and completely unhighlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Colour {
    /// One of the 256, however it was written — `38;5;N`, `30`-`37` or `90`-`97`.
    Indexed(u8),
    /// A 24-bit colour, `38;2;R;G;B`.
    Rgb(u8, u8, u8),
}

/// Which role a foreground colour means.
///
/// Empty by default, and filled from configuration. This is the join between a program that
/// speaks ANSI — `bat`, `delta`, `rg --color=always` — and a vocabulary that speaks meaning: the
/// mapping is a property of *that program's theme*, so it belongs in a table somebody can edit
/// rather than in a match nobody can reach.
pub type Theme = std::collections::BTreeMap<Colour, Role>;

/// Read ANSI-coloured output into painted lines.
///
/// **The escapes always go.** What varies is whether anything is learned from them: a colour the
/// theme names becomes a role, and one it does not becomes [`Role::Text`]. So a tool with no
/// theme at all still comes out legible and unstyled rather than as a screenful of `ESC[38;5;81m`,
/// which is what happens when a harness is handed ANSI it did not expect.
///
/// Only SGR — `ESC [ … m` — is interpreted. Every other escape is dropped: a program that moves
/// the cursor or clears the screen is doing something a transcript has no room for, and passing
/// it through would let a tool paint over the conversation.
#[must_use]
pub fn ansi(text: &str, theme: &Theme) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut line: Line = Vec::new();
        let mut role = Role::Text;
        let mut held = String::new();
        let mut rest = raw;
        while let Some(at) = rest.find('\u{1b}') {
            let (before, after) = rest.split_at(at);
            held.push_str(before);
            let Some((codes, tail)) = escape(after) else {
                // An escape that never ends: everything after it is uninterpretable, so it is
                // dropped rather than shown as the noise it would be.
                rest = "";
                break;
            };
            if let Some(next) = codes.and_then(|codes| of(codes, theme))
                && next != role
            {
                // The run ends where its meaning does, so a span carries one role and a reader
                // is not asked to split it again.
                if !held.is_empty() {
                    line.push(Span::new(role, std::mem::take(&mut held)));
                }
                role = next;
            }
            rest = tail;
        }
        held.push_str(rest);
        // **A line always has at least one span, even an empty one.** A blank line in a file is
        // a line, and a row with no spans is not merely odd: an empty Lua table encodes as `{}`,
        // which is a map where the wire wants a list, so one blank line in a highlighted file
        // made the whole result unreadable on the far side.
        if !held.is_empty() || line.is_empty() {
            line.push(Span::new(role, held));
        }
        out.push(line);
    }
    out
}

/// Split one escape off the front, answering its SGR parameters when it has any.
///
/// `Some(None)` is an escape that is not SGR — recognised, and skipped. `None` is a sequence with
/// no terminator, which cannot be skipped because nothing says where it ends.
fn escape(at: &str) -> Option<(Option<&str>, &str)> {
    let rest = at.strip_prefix('\u{1b}')?;
    let Some(rest) = rest.strip_prefix('[') else {
        // Not CSI. Two-character escapes are the common case; skipping one character is right
        // for those and harmless for the rest, which lose a byte of text they were not going to
        // be read for anyway.
        return Some((None, rest.get(1..).unwrap_or("")));
    };
    let end = rest.find(|c: char| c.is_ascii_alphabetic())?;
    let (params, tail) = rest.split_at(end);
    let (kind, tail) = tail.split_at(1);
    Some(((kind == "m").then_some(params), tail))
}

/// The role an SGR parameter list means, given a theme.
///
/// Three answers, not two. `Some(Text)` is a reset, or a colour the theme does not name — both
/// of which mean "ordinary from here". `None` is a sequence that says nothing about the
/// foreground at all: bold, italic, a background. Those must leave the current role alone, or a
/// `bold` in the middle of a keyword ends it and one word comes out as three spans.
fn of(params: &str, theme: &Theme) -> Option<Role> {
    let codes: Vec<&str> = params.split(';').collect();
    if params.is_empty() || codes.first() == Some(&"0") || codes.first() == Some(&"") {
        return Some(Role::Text);
    }
    let colour = colour(&codes)?;
    Some(theme.get(&colour).copied().unwrap_or(Role::Text))
}

/// The foreground an SGR parameter list asks for, if it asks for one.
///
/// `None` for anything that is not a foreground — bold, italic, a background — so those neither
/// look up nor reset, and a `bold` in the middle of a keyword does not end it.
fn colour(codes: &[&str]) -> Option<Colour> {
    let number = |at: usize| codes.get(at).and_then(|n| n.parse::<u8>().ok());
    match (codes.first(), codes.get(1)) {
        // Truecolour, which is what `bat` and `delta` emit by default.
        (Some(&"38"), Some(&"2")) => Some(Colour::Rgb(number(2)?, number(3)?, number(4)?)),
        // The 256.
        (Some(&"38"), Some(&"5")) => Some(Colour::Indexed(number(2)?)),
        // The basic eight and the bright eight, which are the first sixteen of the 256 — so one
        // table covers every way of naming them.
        _ => match number(0)? {
            code @ 30..=37 => Some(Colour::Indexed(code - 30)),
            code @ 90..=97 => Some(Colour::Indexed(code - 90 + 8)),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of a painted document, which is what must survive whatever the roles do.
    fn text_of(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_diff_reads_as_three_things_rather_than_two_and_a_lie() {
        let painted = diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-was\n+now\n unchanged");
        let roles: Vec<Role> = painted.iter().map(|line| line[0].role).collect();
        assert_eq!(
            roles,
            vec![
                Role::Marker,
                Role::Marker,
                Role::Marker,
                Role::Removed,
                Role::Added,
                Role::Context
            ]
        );
    }

    #[test]
    fn the_escapes_always_go_even_with_no_theme_at_all() {
        // The floor. A tool whose theme nobody has written still comes out legible: unstyled,
        // never a screenful of `ESC[38;5;81m`, which is what a harness handed raw ANSI shows.
        let painted = ansi("\u{1b}[38;5;81mfn\u{1b}[0m main", &Theme::new());
        assert_eq!(text_of(&painted), "fn main");
        assert!(painted[0].iter().all(|span| span.role == Role::Text));
    }

    #[test]
    fn a_colour_the_theme_names_becomes_the_role_it_means() {
        let theme = Theme::from([
            (Colour::Indexed(81), Role::Keyword),
            (Colour::Indexed(114), Role::String),
        ]);
        let painted = ansi("\u{1b}[38;5;81mfn\u{1b}[0m \u{1b}[38;5;114m\"hi\"", &theme);
        let spans = &painted[0];
        assert_eq!(spans[0], Span::new(Role::Keyword, "fn"));
        assert_eq!(spans[1], Span::new(Role::Text, " "));
        assert_eq!(spans[2], Span::new(Role::String, "\"hi\""));
    }

    #[test]
    fn a_colour_it_does_not_name_is_text_rather_than_a_guess() {
        // Guessing would put a colour on screen that the palette never chose, which is the one
        // thing roles exist to prevent.
        let theme = Theme::from([(Colour::Indexed(81), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;5;200mwhat\u{1b}[0m", &theme);
        assert_eq!(painted[0][0], Span::new(Role::Text, "what"));
    }

    #[test]
    fn a_run_carries_one_role_rather_than_being_split_per_escape() {
        // Two escapes saying the same colour is one run of meaning, and a reader should not have
        // to join it back together.
        let theme = Theme::from([(Colour::Indexed(81), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;5;81mfn\u{1b}[38;5;81m x", &theme);
        assert_eq!(painted[0].len(), 1, "{:?}", painted[0]);
        assert_eq!(painted[0][0], Span::new(Role::Keyword, "fn x"));
    }

    #[test]
    fn truecolour_is_read_because_that_is_what_bat_sends() {
        // The gap this was written to close. `bat` emits `38;2;R;G;B` by default, and a parser
        // that only knew `38;5;N` learned nothing from any of it — so a whole file came back
        // legible and completely unhighlighted, which reads as a theme nobody configured rather
        // than as a parser that could not follow the output.
        let theme = Theme::from([(Colour::Rgb(0xc6, 0x78, 0xdd), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;2;198;120;221mfn\u{1b}[0m x", &theme);
        assert_eq!(painted[0][0], Span::new(Role::Keyword, "fn"));
        assert_eq!(painted[0][1], Span::new(Role::Text, " x"));
    }

    #[test]
    fn a_style_that_is_not_a_colour_does_not_end_the_run_it_is_in() {
        // A `bold` in the middle of a keyword is still the keyword. Treating every SGR as a
        // colour would reset the role and split one word into three spans.
        let theme = Theme::from([(Colour::Indexed(81), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;5;81mfn\u{1b}[1m x", &theme);
        assert_eq!(painted[0].len(), 1, "{:?}", painted[0]);
        assert_eq!(painted[0][0].role, Role::Keyword);
    }

    #[test]
    fn the_basic_and_bright_colours_look_up_beside_the_256_ones() {
        // One table for all three, or a theme has to name the same colour three ways.
        let theme = Theme::from([
            (Colour::Indexed(1), Role::Error),
            (Colour::Indexed(12), Role::Path),
        ]);
        assert_eq!(ansi("\u{1b}[31mno", &theme)[0][0].role, Role::Error);
        assert_eq!(ansi("\u{1b}[94m/tmp", &theme)[0][0].role, Role::Path);
    }

    #[test]
    fn moving_the_cursor_is_dropped_rather_than_passed_on() {
        // A tool that cleared the screen would be painting over the conversation. Only SGR is
        // interpreted; everything else loses its escape and keeps its text.
        let painted = ansi("a\u{1b}[2Jb\u{1b}[Hc", &Theme::new());
        assert_eq!(text_of(&painted), "abc");
    }

    #[test]
    fn an_escape_that_never_ends_takes_no_text_with_it_that_it_should_not() {
        // Truncated output is ordinary — a pipe closed mid-sequence — and it must not panic or
        // spill the raw bytes onto the screen.
        let painted = ansi("kept\u{1b}[38;5;81", &Theme::new());
        assert_eq!(text_of(&painted), "kept");
    }

    #[test]
    fn a_blank_line_is_still_a_line_with_a_span_on_it() {
        // Not a nicety. A row with no spans encodes from Lua as `{}` — a map where the wire
        // wants a list — so one blank line in a highlighted file made the whole result
        // unreadable on the far side, and the tool reported "answered something unreadable".
        let painted = ansi("one\n\ntwo", &Theme::new());
        assert_eq!(painted.len(), 3);
        assert_eq!(painted[1], vec![Span::new(Role::Text, "")]);
        assert!(painted.iter().all(|line| !line.is_empty()));
    }

    #[test]
    fn a_line_that_is_nothing_but_escapes_is_still_a_line() {
        // A reset on its own row is ordinary in themed output, and it must not vanish: a line
        // that disappeared would shift every line number after it.
        let painted = ansi("a\n\u{1b}[0m\nb", &Theme::new());
        assert_eq!(painted.len(), 3);
        assert_eq!(painted[1], vec![Span::new(Role::Text, "")]);
    }

    #[test]
    fn plain_text_is_one_role_and_every_line_survives() {
        let painted = plain("one\ntwo\n\nfour");
        assert_eq!(painted.len(), 4);
        assert_eq!(painted[2], vec![Span::new(Role::Text, "")]);
        assert_eq!(text_of(&painted), "one\ntwo\n\nfour");
    }

    #[test]
    fn every_role_travels_by_the_name_the_other_side_reads() {
        // Both siblings own their own copy of this vocabulary, so nothing but a test holds the
        // two together. A renamed variant is a role that silently becomes `text` over there.
        for (role, name) in [
            (Role::Added, "added"),
            (Role::Removed, "removed"),
            (Role::Marker, "marker"),
            (Role::Context, "context"),
            (Role::Keyword, "keyword"),
            (Role::Comment, "comment"),
            (Role::Path, "path"),
            (Role::Error, "error"),
            (Role::Text, "text"),
        ] {
            let wire = serde_json::to_string(&role).expect("encodes");
            assert_eq!(wire, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<Role>(&wire).expect("decodes"), role);
        }
    }
}
