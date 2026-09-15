//! One instruction, spelled two ways, and the emulator only knows one of them.
//!
//! `ESC [ row ; col H` (CUP) and `ESC [ row ; col f` (HVP) put the cursor in the same place on a
//! terminal that has not set margins, which is this one. [`vt100`] implements `H` and silently
//! drops `f`, so the byte goes past here and comes out as the one the emulator knows.
//!
//! A state machine rather than a search: the pty splits a sequence across reads whenever it feels
//! like it, and an `f` in a program's output is a letter.

/// Where in an escape sequence the stream currently is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Where {
    #[default]
    Text,
    /// An `ESC` has arrived and what follows decides whether this is a CSI at all.
    Escape,
    /// Inside `ESC [ … `, collecting parameters until the byte that says what it is.
    Params,
}

/// Rewrites `ESC [ … f` into `ESC [ … H`, across however many reads it takes.
#[derive(Debug, Default)]
pub struct Rewriting {
    at: Where,
    /// Whether the parameters so far are the plain numeric kind HVP takes. A private or
    /// intermediate byte makes it some other instruction that happens to end in `f`.
    plain: bool,
}

impl Rewriting {
    /// Nothing seen yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fix up one chunk in place. The rewrite is one byte for one byte.
    pub fn apply(&mut self, chunk: &mut [u8]) {
        for byte in chunk {
            self.at = match self.at {
                Where::Text if *byte == 0x1b => Where::Escape,
                Where::Text => Where::Text,
                // `ESC [` is the only thing this cares about; every other escape goes untouched.
                Where::Escape => match *byte {
                    b'[' => {
                        self.plain = true;
                        Where::Params
                    }
                    0x1b => Where::Escape,
                    _ => Where::Text,
                },
                Where::Params => match *byte {
                    b'0'..=b'9' | b';' => Where::Params,
                    // The rest of the parameter range and the intermediate range after it: no
                    // longer HVP.
                    0x20..=0x3f => {
                        self.plain = false;
                        Where::Params
                    }
                    0x40..=0x7e => {
                        if *byte == b'f' && self.plain {
                            *byte = b'H';
                        }
                        Where::Text
                    }
                    // `ESC` restarts; a stray control byte abandons the sequence rather than
                    // swallowing everything printed after it.
                    0x1b => Where::Escape,
                    _ => Where::Text,
                },
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bytes` after the rewrite, as one chunk.
    fn once(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        Rewriting::new().apply(&mut out);
        out
    }

    #[test]
    fn the_position_btop_asks_for_becomes_the_one_the_emulator_knows() {
        assert_eq!(once(b"\x1b[3;5f"), b"\x1b[3;5H");
        // No parameters at all is HVP's "go home", and `H` means the same with none.
        assert_eq!(once(b"\x1b[f"), b"\x1b[H");
    }

    #[test]
    fn everything_else_crosses_untouched() {
        for whole in [
            &b"\x1b[3;5H"[..],
            b"\x1b[1;31;42m",
            b"\x1b[2J",
            b"\x1b[?25l",
            b"\x1b[?1049h",
            b"plain text with an f in it",
            b"\x1b(B",
        ] {
            assert_eq!(once(whole), whole, "{:?}", String::from_utf8_lossy(whole));
        }
    }

    #[test]
    fn an_f_that_is_a_letter_is_left_alone() {
        assert_eq!(once(b"the quick brown fox"), b"the quick brown fox");
        assert_eq!(once(b"\x1b[31mfff\x1b[m"), b"\x1b[31mfff\x1b[m");
    }

    #[test]
    fn a_private_sequence_is_not_a_position() {
        assert_eq!(once(b"\x1b[?7f"), b"\x1b[?7f");
        assert_eq!(once(b"\x1b[3 f"), b"\x1b[3 f");
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_one_sequence() {
        let mut fixing = Rewriting::new();
        let mut first = b"text\x1b[3;".to_vec();
        let mut second = b"5ftext".to_vec();
        fixing.apply(&mut first);
        fixing.apply(&mut second);
        assert_eq!([first, second].concat(), b"text\x1b[3;5Htext");
    }

    #[test]
    fn a_sequence_split_at_the_escape_survives_it() {
        let mut fixing = Rewriting::new();
        let mut first = b"\x1b".to_vec();
        let mut second = b"[9;9f".to_vec();
        fixing.apply(&mut first);
        fixing.apply(&mut second);
        assert_eq!([first, second].concat(), b"\x1b[9;9H");
    }

    #[test]
    fn a_sequence_abandoned_part_way_does_not_swallow_what_follows() {
        let mut fixing = Rewriting::new();
        let mut out = b"\x1b[3\x07f".to_vec();
        fixing.apply(&mut out);
        assert_eq!(out, b"\x1b[3\x07f");
    }

    #[test]
    fn an_escape_inside_a_sequence_starts_the_next_one() {
        assert_eq!(once(b"\x1b[3\x1b[4;4f"), b"\x1b[3\x1b[4;4H");
    }
}
