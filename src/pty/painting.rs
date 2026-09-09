//! A grid of terminal cells, as rows of spans.
//!
//! The one place a picture is allowed its own colours. A program running on a pty has already
//! decided what its output looks like, so indexed and RGB colours cross as RGB; only a cell left
//! at the terminal's *default* colour is handed over as a role, and so comes out in the reader's
//! own theme.

use crate::paint::{Line, Role, Span};

/// The whole screen, as rows of spans, with the cursor if it is showing.
///
/// Runs of identical styling are merged: a row of eighty cells is one span rather than eighty.
pub fn rows(screen: &vt100::Screen) -> Vec<Line> {
    let (height, width) = screen.size();
    (0..height)
        .map(|row| {
            let mut out: Line = Vec::new();
            for col in 0..width {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                // A wide character's second half is not a cell of its own: emitting a space here
                // would push the rest of the row along by one.
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                let painted = paint(cell);
                match out.last_mut() {
                    Some(last) if same(last, &painted) => last.text.push_str(text),
                    _ => out.push(Span {
                        text: text.to_owned(),
                        ..painted
                    }),
                }
            }
            out
        })
        .collect()
}

/// Where the cursor is, or `None` when the program has hidden it.
#[must_use]
pub fn cursor(screen: &vt100::Screen) -> Option<crate::tools::At> {
    if screen.hide_cursor() {
        return None;
    }
    let (row, col) = screen.cursor_position();
    Some(crate::tools::At { row, col })
}

/// One cell's styling, with no text in it yet.
fn paint(cell: &vt100::Cell) -> Span {
    let mut fg = colour(cell.fgcolor(), cell.bold());
    let mut bg = colour(cell.bgcolor(), false);
    // Inverse is a swap and nothing else, which is what a terminal does with it.
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
        // A swap only works when both sides are known. Reversed text on default colours says
        // "dim on nothing" and the harness fills it from the theme.
        if fg.is_none() && bg.is_none() {
            return Span {
                role: Role::Dim,
                text: String::new(),
                rgb: None,
                bg: None,
            };
        }
    }
    Span {
        // Carries only when `rgb` is `None`, which is the program having left the colour alone.
        role: Role::Text,
        text: String::new(),
        rgb: fg,
        bg,
    }
}

/// Whether two spans are styled alike, so their text can be one run.
fn same(a: &Span, b: &Span) -> bool {
    a.role == b.role && a.rgb == b.rgb && a.bg == b.bg
}

/// One of the terminal's colours as an actual colour.
///
/// `None` for the default, which is the only case that stays a role; an index is resolved here.
fn colour(from: vt100::Color, bold: bool) -> Option<[u8; 3]> {
    match from {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some([r, g, b]),
        // Bold on one of the first eight means the bright one, which programs still rely on.
        vt100::Color::Idx(n) => Some(indexed(if bold && n < 8 { n + 8 } else { n })),
    }
}

/// The xterm 256-colour palette: sixteen named colours, then a 6×6×6 cube, then a grey ramp. Only
/// the first sixteen are a table, and these are the values xterm itself uses.
fn indexed(n: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    /// What each step of the cube is worth. Not evenly spaced: a linear ramp here makes every
    /// dark colour visibly wrong.
    const STEP: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match n {
        0..=15 => BASE[n as usize],
        16..=231 => {
            let n = n - 16;
            [
                STEP[(n / 36) as usize],
                STEP[(n / 6 % 6) as usize],
                STEP[(n % 6) as usize],
            ]
        }
        // Twenty-four greys from just off black to just off white.
        grey => {
            let level = 8 + (grey - 232) * 10;
            [level, level, level]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen `cols` wide with `bytes` written into it.
    fn played(cols: u16, bytes: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(3, cols, 0);
        parser.process(bytes);
        parser
    }

    fn text(line: &Line) -> String {
        line.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn what_the_program_wrote_is_what_the_row_says() {
        let parser = played(10, b"hello");
        let drawn = rows(parser.screen());
        assert_eq!(text(&drawn[0]), "hello     ", "padded to the width");
        assert_eq!(drawn.len(), 3, "every row, drawn or not");
    }

    #[test]
    fn a_run_of_one_styling_is_one_span() {
        let parser = played(10, b"hello");
        let drawn = rows(parser.screen());
        assert_eq!(drawn[0].len(), 1, "{:?}", drawn[0]);
    }

    #[test]
    fn a_colour_the_program_chose_crosses_as_that_colour() {
        let parser = played(10, b"\x1b[31mred\x1b[m.");
        let drawn = rows(parser.screen());
        assert_eq!(drawn[0][0].text, "red");
        assert_eq!(drawn[0][0].rgb, Some([205, 0, 0]));
        assert_eq!(drawn[0][1].rgb, None);
        assert_eq!(drawn[0][1].role, Role::Text);
    }

    #[test]
    fn truecolour_crosses_untouched() {
        let parser = played(6, b"\x1b[38;2;12;34;56mx");
        assert_eq!(rows(parser.screen())[0][0].rgb, Some([12, 34, 56]));
    }

    #[test]
    fn bold_on_one_of_the_first_eight_is_the_bright_one() {
        let parser = played(6, b"\x1b[1;31mx");
        assert_eq!(rows(parser.screen())[0][0].rgb, Some([255, 0, 0]));
    }

    #[test]
    fn inverse_swaps_the_two_sides() {
        let parser = played(6, b"\x1b[31;44;7mx");
        let span = &rows(parser.screen())[0][0];
        assert_eq!(span.rgb, Some([0, 0, 238]), "the background became the ink");
        assert_eq!(span.bg, Some([205, 0, 0]), "and the ink the background");
    }

    #[test]
    fn the_cursor_is_where_the_program_left_it() {
        let parser = played(10, b"abc");
        assert_eq!(
            cursor(parser.screen()),
            Some(crate::tools::At { row: 0, col: 3 })
        );
        let parser = played(10, b"abc\x1b[?25l");
        assert_eq!(cursor(parser.screen()), None);
    }

    #[test]
    fn the_cube_and_the_greys_are_arithmetic() {
        assert_eq!(indexed(16), [0, 0, 0], "the corner of the cube");
        assert_eq!(indexed(231), [255, 255, 255], "and the far one");
        assert_eq!(indexed(232), [8, 8, 8], "the first grey");
        assert_eq!(indexed(255), [238, 238, 238], "and the last");
    }
}
