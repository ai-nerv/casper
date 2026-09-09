//! What a tool's output means, which is what magi turns into a colour. A tool names a [`Role`] —
//! `added`, `keyword`, `path` — and the harness resolves it against its own palette. magi keeps
//! its own copy of these types in `magi-proto::tooling`; the JSON is the contract, pinned by
//! round-trip tests on both sides.

use serde::{Deserialize, Serialize};

/// What a span of text is. Closed on purpose: a tool naming its own role would be asking the
/// harness to invent a colour for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Text,
    /// Present but secondary.
    Muted,
    /// Present and nearly out of the way.
    Dim,
    Title,
    /// A path, a filename, a location.
    Path,
    Ok,
    Warn,
    Error,
    Added,
    Removed,
    /// The `@@` and `+++` rows, which say *where* rather than what.
    Marker,
    /// A line a patch leaves alone.
    Context,
    Keyword,
    String,
    Number,
    Comment,
    /// A type name.
    Type,
    /// A function name.
    Func,
}

/// A run of text with one meaning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    #[serde(default)]
    pub role: Role,
    pub text: String,
    /// A colour chosen outright, overriding the role. Surfaces drawing a picture may use this;
    /// tool output, which is read alongside everything else, should not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    /// A background chosen outright, for the same narrow reason as [`Span::rgb`]. What makes a run
    /// read as inverted rather than merely coloured.
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

/// Paint a unified diff. The `+++`, `---` and `@@` rows are neither added nor removed — they say
/// where — so they get a role of their own.
#[must_use]
pub fn diff(text: &str) -> Vec<Line> {
    text.lines()
        .map(|line| {
            // Headers first: they open with the characters an added or removed line does.
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
#[must_use]
pub fn plain(text: &str) -> Vec<Line> {
    text.lines()
        .map(|line| vec![Span::new(Role::Text, line)])
        .collect()
}

/// A foreground colour, as a program actually asked for it. Both forms, because both are in use:
/// `bat` emits truecolour by default and a theme that only spoke indices would match none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Colour {
    /// One of the 256, however it was written — `38;5;N`, `30`-`37` or `90`-`97`.
    Indexed(u8),
    /// A 24-bit colour, `38;2;R;G;B`.
    Rgb(u8, u8, u8),
}

/// Which role a foreground colour means. Empty by default and filled from configuration: the
/// mapping is a property of that program's theme, so it belongs in a table somebody can edit.
pub type Theme = std::collections::BTreeMap<Colour, Role>;

/// Read ANSI-coloured output into painted lines. The escapes always go; a colour the theme names
/// becomes a role, and one it does not becomes [`Role::Text`]. Only SGR — `ESC [ … m` — is
/// interpreted, and every other escape is dropped rather than let a tool paint over the
/// conversation.
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
                // An escape that never ends: everything after it is uninterpretable.
                rest = "";
                break;
            };
            if let Some(next) = codes.and_then(|codes| of(codes, theme))
                && next != role
            {
                // The run ends where its meaning does, so a span carries one role.
                if !held.is_empty() {
                    line.push(Span::new(role, std::mem::take(&mut held)));
                }
                role = next;
            }
            rest = tail;
        }
        held.push_str(rest);
        // A line always has at least one span, even an empty one: a row with no spans encodes
        // from Lua as `{}`, a map where the wire wants a list.
        if !held.is_empty() || line.is_empty() {
            line.push(Span::new(role, held));
        }
        out.push(line);
    }
    out
}

/// Split one escape off the front, answering its SGR parameters when it has any. `Some(None)` is
/// an escape that is not SGR. `None` is a sequence with no terminator, which cannot be skipped
/// because nothing says where it ends.
fn escape(at: &str) -> Option<(Option<&str>, &str)> {
    let rest = at.strip_prefix('\u{1b}')?;
    let Some(rest) = rest.strip_prefix('[') else {
        // Not CSI: skipping one character is right for a two-character escape.
        return Some((None, rest.get(1..).unwrap_or("")));
    };
    let end = rest.find(|c: char| c.is_ascii_alphabetic())?;
    let (params, tail) = rest.split_at(end);
    let (kind, tail) = tail.split_at(1);
    Some(((kind == "m").then_some(params), tail))
}

/// The role an SGR parameter list means, given a theme. Three answers, not two: `Some(Text)` is a
/// reset or a colour the theme does not name, and `None` is a sequence saying nothing about the
/// foreground — bold, italic, a background — which must leave the current role alone.
fn of(params: &str, theme: &Theme) -> Option<Role> {
    let codes: Vec<&str> = params.split(';').collect();
    if params.is_empty() || codes.first() == Some(&"0") || codes.first() == Some(&"") {
        return Some(Role::Text);
    }
    let colour = colour(&codes)?;
    Some(theme.get(&colour).copied().unwrap_or(Role::Text))
}

/// The foreground an SGR parameter list asks for, if it asks for one. `None` for anything that is
/// not a foreground, so those neither look up nor reset.
fn colour(codes: &[&str]) -> Option<Colour> {
    let number = |at: usize| codes.get(at).and_then(|n| n.parse::<u8>().ok());
    match (codes.first(), codes.get(1)) {
        // Truecolour, which is what `bat` and `delta` emit by default.
        (Some(&"38"), Some(&"2")) => Some(Colour::Rgb(number(2)?, number(3)?, number(4)?)),
        (Some(&"38"), Some(&"5")) => Some(Colour::Indexed(number(2)?)),
        // The basic and bright eight are the first sixteen of the 256, so one table covers all.
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
        let theme = Theme::from([(Colour::Indexed(81), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;5;200mwhat\u{1b}[0m", &theme);
        assert_eq!(painted[0][0], Span::new(Role::Text, "what"));
    }

    #[test]
    fn a_run_carries_one_role_rather_than_being_split_per_escape() {
        let theme = Theme::from([(Colour::Indexed(81), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;5;81mfn\u{1b}[38;5;81m x", &theme);
        assert_eq!(painted[0].len(), 1, "{:?}", painted[0]);
        assert_eq!(painted[0][0], Span::new(Role::Keyword, "fn x"));
    }

    #[test]
    fn truecolour_is_read_because_that_is_what_bat_sends() {
        let theme = Theme::from([(Colour::Rgb(0xc6, 0x78, 0xdd), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;2;198;120;221mfn\u{1b}[0m x", &theme);
        assert_eq!(painted[0][0], Span::new(Role::Keyword, "fn"));
        assert_eq!(painted[0][1], Span::new(Role::Text, " x"));
    }

    #[test]
    fn a_style_that_is_not_a_colour_does_not_end_the_run_it_is_in() {
        let theme = Theme::from([(Colour::Indexed(81), Role::Keyword)]);
        let painted = ansi("\u{1b}[38;5;81mfn\u{1b}[1m x", &theme);
        assert_eq!(painted[0].len(), 1, "{:?}", painted[0]);
        assert_eq!(painted[0][0].role, Role::Keyword);
    }

    #[test]
    fn the_basic_and_bright_colours_look_up_beside_the_256_ones() {
        let theme = Theme::from([
            (Colour::Indexed(1), Role::Error),
            (Colour::Indexed(12), Role::Path),
        ]);
        assert_eq!(ansi("\u{1b}[31mno", &theme)[0][0].role, Role::Error);
        assert_eq!(ansi("\u{1b}[94m/tmp", &theme)[0][0].role, Role::Path);
    }

    #[test]
    fn moving_the_cursor_is_dropped_rather_than_passed_on() {
        let painted = ansi("a\u{1b}[2Jb\u{1b}[Hc", &Theme::new());
        assert_eq!(text_of(&painted), "abc");
    }

    #[test]
    fn an_escape_that_never_ends_takes_no_text_with_it_that_it_should_not() {
        let painted = ansi("kept\u{1b}[38;5;81", &Theme::new());
        assert_eq!(text_of(&painted), "kept");
    }

    #[test]
    fn a_blank_line_is_still_a_line_with_a_span_on_it() {
        let painted = ansi("one\n\ntwo", &Theme::new());
        assert_eq!(painted.len(), 3);
        assert_eq!(painted[1], vec![Span::new(Role::Text, "")]);
        assert!(painted.iter().all(|line| !line.is_empty()));
    }

    #[test]
    fn a_line_that_is_nothing_but_escapes_is_still_a_line() {
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
