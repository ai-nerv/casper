//! Code in its own language, as roles. syntect reads the grammar from Sublime's regex definitions,
//! and each kind of token is named as a role rather than a colour, so the reader's palette decides
//! how it looks and a highlighted file matches everything else on the screen.

use crate::paint::{Line, Role, Span};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Which role each kind of token is; syntect takes the most specific scope that matches.
const KINDS: [(&str, Role); 6] = [
    ("comment", Role::Comment),
    ("string, constant.character", Role::String),
    (
        "constant.numeric, constant.language, constant.other",
        Role::Number,
    ),
    ("keyword, storage", Role::Keyword),
    (
        "entity.name.function, support.function, variable.function, entity.name.tag",
        Role::Func,
    ),
    (
        "entity.name.type, entity.name.class, support.type, support.class",
        Role::Type,
    ),
];

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// A theme whose every colour is a role in disguise: the red channel is the role's place in
/// [`KINDS`], and what no rule names is past the end of it, which is plain text.
fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme {
        settings: ThemeSettings {
            foreground: Some(tag(KINDS.len())),
            ..ThemeSettings::default()
        },
        scopes: KINDS
            .iter()
            .enumerate()
            .filter_map(|(nth, (scope, _))| {
                Some(ThemeItem {
                    scope: scope.parse::<ScopeSelectors>().ok()?,
                    style: StyleModifier {
                        foreground: Some(tag(nth)),
                        background: None,
                        font_style: None,
                    },
                })
            })
            .collect(),
        ..Theme::default()
    })
}

fn tag(nth: usize) -> Color {
    Color {
        r: u8::try_from(nth).unwrap_or(u8::MAX),
        g: 0,
        b: 0,
        a: 0xff,
    }
}

/// The language a file is in: by its extension, or by its whole name for one like `Makefile`.
fn syntax_for(file: &str) -> Option<&'static SyntaxReference> {
    let name = file.rsplit('/').next()?;
    let token = name.rsplit_once('.').map_or(name, |(_, ext)| ext);
    syntaxes().find_syntax_by_token(token)
}

/// `lines` of `file`, each as runs of one role. Highlighted as one run, so what spans lines — a
/// block comment, a long string — stays coloured. `None` for a language syntect does not know.
fn roles(file: &str, lines: &[&str]) -> Option<Vec<Line>> {
    let mut highlighter = HighlightLines::new(syntax_for(file)?, theme());
    lines
        .iter()
        .map(|line| {
            let ended = format!("{line}\n");
            let ranges = highlighter.highlight_line(&ended, syntaxes()).ok()?;
            let mut out: Line = Vec::new();
            for (style, text) in ranges {
                let text = text.trim_end_matches('\n');
                if text.is_empty() {
                    continue;
                }
                let role = KINDS
                    .get(usize::from(style.foreground.r))
                    .map_or(Role::Text, |(_, role)| *role);
                match out.last_mut() {
                    Some(last) if last.role == role => last.text.push_str(text),
                    _ => out.push(Span::new(role, text)),
                }
            }
            if out.is_empty() {
                out.push(Span::new(Role::Text, ""));
            }
            Some(out)
        })
        .collect()
}

/// A file's text, highlighted in its language; plain where the language is not one syntect knows.
#[must_use]
pub fn code(text: &str, file: &str) -> Vec<Line> {
    let lines: Vec<&str> = text.lines().collect();
    roles(file, &lines).unwrap_or_else(|| crate::paint::plain(text))
}

/// A unified diff of `file`: the code in its language, and each changed line on the ground of what
/// happened to it — `added`, `removed`, or `changed` for new lines straight after removed ones.
#[must_use]
pub fn diff(text: &str, file: &str) -> Vec<Line> {
    let rows: Vec<&str> = text.lines().collect();
    // Headings are the file names before the first hunk, and each hunk's `@@`: a removed line that
    // happens to start `--` is still a removed line.
    let mut in_hunk = false;
    let headings: Vec<bool> = rows
        .iter()
        .map(|row| {
            if row.starts_with("@@") {
                in_hunk = true;
                return true;
            }
            !in_hunk && (row.starts_with("---") || row.starts_with("+++"))
        })
        .collect();
    let code: Vec<&str> = rows
        .iter()
        .zip(&headings)
        .filter(|(_, heading)| !**heading)
        .map(|(row, _)| row.get(1..).unwrap_or(""))
        .collect();
    let mut painted = roles(file, &code)
        .unwrap_or_else(|| {
            code.iter()
                .map(|line| vec![Span::new(Role::Text, *line)])
                .collect()
        })
        .into_iter();
    let mut after_removed = false;
    rows.iter()
        .zip(&headings)
        .map(|(row, heading)| {
            if *heading {
                after_removed = false;
                return vec![Span::new(Role::Marker, *row)];
            }
            let back = match row.as_bytes().first() {
                Some(b'-') => Some(Role::Removed),
                Some(b'+') if after_removed => Some(Role::Changed),
                Some(b'+') => Some(Role::Added),
                _ => None,
            };
            after_removed = matches!(back, Some(Role::Removed | Role::Changed));
            let mark = Span::new(back.unwrap_or(Role::Context), row.get(..1).unwrap_or(""));
            std::iter::once(mark)
                .chain(painted.next().unwrap_or_default())
                .map(|span| Span { back, ..span })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn a_file_reads_as_the_roles_of_its_language() {
        let lines = code("fn main() { let x = 1; } // done", "src/main.rs");
        let roles: Vec<Role> = lines[0].iter().map(|span| span.role).collect();
        for wanted in [Role::Keyword, Role::Number, Role::Comment] {
            assert!(roles.contains(&wanted), "no {wanted:?} in {:?}", lines[0]);
        }
        assert_eq!(text(&lines[0]), "fn main() { let x = 1; } // done");
    }

    #[test]
    fn a_language_nobody_knows_is_plain() {
        assert_eq!(code("a = b", "notes.zzz"), crate::paint::plain("a = b"));
    }

    #[test]
    fn a_diff_keeps_its_code_highlighted_on_the_ground_of_each_change() {
        let lines = diff(
            "--- a.rs\n+++ a.rs\n@@ -1,3 +1,3 @@\n fn a() {}\n-let x = 1;\n+let x = 2;\n+let y = 3;",
            "a.rs",
        );
        let back = |n: usize| lines[n].iter().map(|span| span.back).collect::<Vec<_>>();
        assert!(lines[..3].iter().all(|line| line[0].role == Role::Marker));
        assert!(
            back(3).iter().all(Option::is_none),
            "context sits on nothing"
        );
        assert!(back(4).iter().all(|b| *b == Some(Role::Removed)));
        assert!(
            back(5).iter().all(|b| *b == Some(Role::Changed)),
            "new after removed"
        );
        assert!(back(6).iter().all(|b| *b == Some(Role::Changed)));
        assert!(
            lines[4].iter().any(|span| span.role == Role::Keyword),
            "still code"
        );
        assert_eq!(text(&lines[5]), "+let x = 2;");
    }

    #[test]
    fn a_removed_lua_comment_is_not_taken_for_a_heading() {
        let lines = diff("@@ -1 +1 @@\n--- old\n+-- new", "x.lua");
        assert_eq!(lines[1][0].back, Some(Role::Removed));
        assert_eq!(lines[2][0].back, Some(Role::Changed));
    }
}
