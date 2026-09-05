//! Every escape sequence the emulator threw away, written down.
//!
//! **The canary.** [`vt100`] implements a subset of what a terminal does, and everything outside
//! it is silently dropped — which is the worst possible failure mode, because the program keeps
//! running and the screen just comes out wrong. `btop` positions with `ESC [ r ; c f` where the
//! emulator only knows `ESC [ r ; c H`, so all 452 of its position commands went nowhere; what
//! that looked like was text wrapping mid-word, and finding the cause meant sniffing a pty and
//! ruling out three wrong theories first.
//!
//! This is that diagnosis, made automatic. vt100 hands every sequence it could not read to a
//! [`vt100::Callbacks`], so the ones that fall through are counted here by name. A screen that
//! renders wrong now says *why* — `dropped CSI f ×452` — instead of leaving somebody to work it
//! out from the shape of the damage.
//!
//! **It reports; it cannot repair.** The callback is handed a `&mut Screen`, but the only public
//! mutators on one are `set_size` and `set_scrollback` — the cursor and the grid are not reachable
//! from out here. So nothing can be acted on at this point, and what a sequence *should* have done
//! is dealt with earlier, on the byte stream, where a rewrite is still possible: see
//! [`super::rewriting`].
//!
//! The other half is the test suite. A conformance sweep asserts that nothing is dropped for the
//! sequences a full-screen program actually uses, so the next gap is a failing test rather than a
//! person reporting that their screen looks odd.

use std::collections::BTreeMap;

/// What was dropped, and how many times.
///
/// Counted rather than logged one by one: a program redraws thirty times a second, so the
/// interesting number is "this sequence, four hundred times" and not four hundred lines of it.
#[derive(Debug, Default, Clone)]
pub struct Noticing {
    dropped: BTreeMap<String, usize>,
}

impl Noticing {
    /// Nothing dropped yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been dropped so far, most frequent first.
    #[must_use]
    pub fn dropped(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = self
            .dropped
            .iter()
            .map(|(what, n)| (what.clone(), *n))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Whether the emulator understood everything it was given.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.dropped.is_empty()
    }

    /// One line naming what was lost, or `None` when nothing was.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        if self.dropped.is_empty() {
            return None;
        }
        let each: Vec<String> = self
            .dropped()
            .into_iter()
            .map(|(what, n)| format!("{what} \u{d7}{n}"))
            .collect();
        Some(format!("the emulator dropped: {}", each.join(", ")))
    }

    fn note(&mut self, what: String) {
        *self.dropped.entry(what).or_default() += 1;
    }
}

/// How a sequence is named, so two of the same kind count as one entry.
///
/// **The parameters are kept for a private sequence and dropped for an ordinary one**, because
/// they mean opposite things. On `CSI 3;5f` they are a row and a column — `CSI 9;9f` is the same
/// gap, and a tally keyed on the numbers would be a line per redraw. On `CSI ?1049h` the number
/// *is* which instruction it is: the alternate screen and the mouse are both `?…h`, and a report
/// that called them one thing would name nothing anybody could act on.
fn named(kind: &str, i1: Option<u8>, i2: Option<u8>, params: &[&[u16]], last: char) -> String {
    let mark = |i: Option<u8>| i.map(|b| (b as char).to_string()).unwrap_or_default();
    let numbers = if i1.is_some() {
        params
            .iter()
            .filter_map(|part| part.first())
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(";")
    } else {
        String::new()
    };
    format!("{kind} {}{numbers}{}{last}", mark(i1), mark(i2))
}

impl vt100::Callbacks for Noticing {
    fn unhandled_csi(
        &mut self,
        _: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        self.note(named("CSI", i1, i2, params, c));
    }

    fn unhandled_escape(&mut self, _: &mut vt100::Screen, i1: Option<u8>, i2: Option<u8>, b: u8) {
        self.note(named("ESC", i1, i2, &[], b as char));
    }

    fn unhandled_control(&mut self, _: &mut vt100::Screen, b: u8) {
        self.note(format!("control 0x{b:02x}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a parser with the canary attached drops for `bytes`.
    fn dropped(bytes: &[u8]) -> Vec<(String, usize)> {
        let mut parser = vt100::Parser::new_with_callbacks(10, 40, 0, Noticing::new());
        parser.process(bytes);
        parser.callbacks().dropped()
    }

    #[test]
    fn a_sequence_the_emulator_cannot_read_is_named() {
        // The one that started this. Without the rewrite in front of it, every `f` lands here.
        assert_eq!(dropped(b"\x1b[3;5f"), [("CSI f".to_owned(), 1)]);
    }

    #[test]
    fn the_same_gap_many_times_is_one_entry_with_a_count() {
        // A program redraws thirty times a second. The useful number is "this sequence, four
        // hundred times", not four hundred lines saying so.
        let dropped = dropped(b"\x1b[1;1f\x1b[2;2f\x1b[9;9f");
        assert_eq!(dropped, [("CSI f".to_owned(), 3)]);
    }

    #[test]
    fn what_the_emulator_does_understand_is_not_reported() {
        // Otherwise the canary is noise and nobody reads it. These are the sequences a full-screen
        // program spends its life in.
        let clean: &[&[u8]] = &[
            b"\x1b[3;5H",                    // cursor position
            b"\x1b[2J\x1b[K",                // erase display, erase line
            b"\x1b[1;31;42m\x1b[0m",         // colours
            b"\x1b[38;2;1;2;3m",             // truecolour
            b"\x1b[?1049h\x1b[?1049l",       // the alternate screen
            b"\x1b[?25l\x1b[?25h",           // hiding the cursor
            b"\x1b[?1000h\x1b[?1006h",       // asking for the mouse
            b"\x1b[5A\x1b[5B\x1b[5C\x1b[5D", // relative moves
            b"\x1b[2L\x1b[2M\x1b[2P\x1b[2X", // insert and delete
            b"\x1b[3;9r",                    // a scrolling region
            b"\x1b7\x1b8",                   // save and restore the cursor
            b"\x1b[10d\x1b[10G",             // absolute row, absolute column
            b"hello\r\n\ttext",              // and ordinary output
        ];
        for one in clean {
            assert_eq!(
                dropped(one),
                [],
                "{:?} is not understood after all",
                String::from_utf8_lossy(one)
            );
        }
    }

    #[test]
    fn the_summary_says_what_was_lost_and_how_often() {
        let mut parser = vt100::Parser::new_with_callbacks(10, 40, 0, Noticing::new());
        parser.process(b"\x1b[1;1f\x1b[2;2f\x1b[4Z");
        let said = parser.callbacks().summary().expect("something was dropped");
        assert!(said.contains("CSI f \u{d7}2"), "{said}");
        assert!(said.contains("CSI Z \u{d7}1"), "{said}");
    }

    #[test]
    fn a_screen_that_understood_everything_says_nothing() {
        let mut parser = vt100::Parser::new_with_callbacks(10, 40, 0, Noticing::new());
        parser.process(b"\x1b[3;5Hhello");
        assert!(parser.callbacks().complete());
        assert_eq!(parser.callbacks().summary(), None);
    }
}
