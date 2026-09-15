//! Turning a key's *name* back into the bytes a terminal would have sent.
//!
//! The inverse of what the harness did to get here: a UI decoded the keypress out of the
//! terminal's escape sequences and passed on the name — see `magi-cli/src/keying.rs` — and a
//! program on a pty expects those sequences and nothing else.
//!
//! An application-cursor program wants `ESC O A` for the up arrow where an ordinary one wants
//! `ESC [ A`, and the screen says which mode it is in.

/// The bytes for `name`, or `None` for a key this cannot express.
///
/// `application` is [`vt100::Screen::application_cursor`].
#[must_use]
pub fn bytes(name: &str, application: bool) -> Option<Vec<u8>> {
    // Modifiers first, outermost in. `alt+` is the escape prefix a terminal sends for a meta key.
    if let Some(rest) = name.strip_prefix("ctrl+") {
        return control(rest);
    }
    if let Some(rest) = name.strip_prefix("alt+") {
        let mut out = vec![0x1b];
        out.extend(bytes(rest, application)?);
        return Some(out);
    }

    let arrow = |last: u8| {
        Some(if application {
            vec![0x1b, b'O', last]
        } else {
            vec![0x1b, b'[', last]
        })
    };
    let tilde = |n: &[u8]| {
        let mut out = vec![0x1b, b'['];
        out.extend_from_slice(n);
        out.push(b'~');
        Some(out)
    };

    match name {
        "space" => Some(vec![b' ']),
        // Carriage return, not newline: `\n` to a program in raw mode is a literal linefeed,
        // which in an editor is a character rather than a key.
        "enter" => Some(vec![b'\r']),
        "tab" => Some(vec![b'\t']),
        "backtab" => Some(vec![0x1b, b'[', b'Z']),
        // DEL rather than BS: a program reading BS treats it as ^H, which in `readline` deletes
        // forward.
        "backspace" => Some(vec![0x7f]),
        "esc" => Some(vec![0x1b]),
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "home" => arrow(b'H'),
        "end" => arrow(b'F'),
        "insert" => tilde(b"2"),
        "delete" => tilde(b"3"),
        "pageup" => tilde(b"5"),
        "pagedown" => tilde(b"6"),
        // The first four predate the numbered form and are what every curses program looks for.
        "f1" => Some(vec![0x1b, b'O', b'P']),
        "f2" => Some(vec![0x1b, b'O', b'Q']),
        "f3" => Some(vec![0x1b, b'O', b'R']),
        "f4" => Some(vec![0x1b, b'O', b'S']),
        "f5" => tilde(b"15"),
        "f6" => tilde(b"17"),
        "f7" => tilde(b"18"),
        "f8" => tilde(b"19"),
        "f9" => tilde(b"20"),
        "f10" => tilde(b"21"),
        "f11" => tilde(b"23"),
        "f12" => tilde(b"24"),
        // Anything else is the character itself.
        other if other.chars().count() == 1 => Some(other.as_bytes().to_vec()),
        _ => None,
    }
}

/// The control code for `ctrl+<something>`: the old ASCII arrangement, a letter's low five bits.
fn control(rest: &str) -> Option<Vec<u8>> {
    let mut chars = rest.chars();
    let (Some(one), None) = (chars.next(), chars.next()) else {
        // `ctrl+enter`, `ctrl+f5` and friends: real terminals disagree, so nothing is invented.
        return None;
    };
    Some(match one.to_ascii_lowercase() {
        c @ 'a'..='z' => vec![c as u8 - b'a' + 1],
        // The four that follow the letters, in order, and the two either side of them.
        '[' => vec![0x1b],
        '\\' => vec![0x1c],
        ']' => vec![0x1d],
        '^' => vec![0x1e],
        '_' | '?' => vec![0x1f],
        '@' | ' ' => vec![0x00],
        _ => return None,
    })
}

/// A click, as the bytes a program that asked for the mouse expects.
///
/// SGR only — `ESC [ < b ; col ; row M|m`. Coordinates arrive zero-based, the way the surface
/// counts its own rows, and go out one-based, the way the protocol does.
#[must_use]
pub fn mouse(
    kind: crate::tools::Pointed,
    button: Option<crate::tools::Button>,
    row: u16,
    col: u16,
) -> Vec<u8> {
    use crate::tools::{Button, Pointed};
    let which = match button {
        None | Some(Button::Left) => 0,
        Some(Button::Middle) => 1,
        Some(Button::Right) => 2,
    };
    // Bit 5 is motion, bit 6 is the wheel; a release is the same button with a trailing `m`.
    let (code, held) = match kind {
        Pointed::Press => (which, true),
        Pointed::Release => (which, false),
        Pointed::Drag => (which + 32, true),
        Pointed::Moved => (35, true),
        Pointed::ScrollUp => (64, true),
        Pointed::ScrollDown => (65, true),
    };
    format!(
        "\x1b[<{code};{};{}{}",
        col + 1,
        row + 1,
        if held { 'M' } else { 'm' }
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letter_is_the_letter() {
        assert_eq!(bytes("a", false), Some(b"a".to_vec()));
        assert_eq!(bytes("Q", false), Some(b"Q".to_vec()));
        assert_eq!(bytes("space", false), Some(b" ".to_vec()));
    }

    #[test]
    fn enter_is_a_carriage_return() {
        assert_eq!(bytes("enter", false), Some(b"\r".to_vec()));
    }

    #[test]
    fn the_arrows_follow_the_mode_the_program_asked_for() {
        assert_eq!(bytes("up", false), Some(b"\x1b[A".to_vec()));
        assert_eq!(bytes("up", true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn control_is_the_letter_low_five_bits() {
        assert_eq!(bytes("ctrl+c", false), Some(vec![3]));
        assert_eq!(bytes("ctrl+a", false), Some(vec![1]));
        // Case does not change a control code: ctrl+shift+c is still ETX to a terminal.
        assert_eq!(bytes("ctrl+C", false), Some(vec![3]));
    }

    #[test]
    fn alt_is_an_escape_in_front_of_it() {
        assert_eq!(bytes("alt+f", false), Some(vec![0x1b, b'f']));
        assert_eq!(bytes("alt+left", false), Some(b"\x1b\x1b[D".to_vec()));
    }

    #[test]
    fn the_function_keys_split_at_five() {
        assert_eq!(bytes("f1", false), Some(b"\x1bOP".to_vec()));
        assert_eq!(bytes("f5", false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(bytes("f12", false), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn a_key_this_cannot_express_sends_nothing() {
        assert_eq!(bytes("ctrl+f5", false), None);
        assert_eq!(bytes("mystery", false), None);
    }

    #[test]
    fn a_click_is_sgr_and_one_based() {
        use crate::tools::{Button, Pointed};
        assert_eq!(
            mouse(Pointed::Press, Some(Button::Left), 0, 0),
            b"\x1b[<0;1;1M".to_vec()
        );
        assert_eq!(
            mouse(Pointed::Release, Some(Button::Left), 2, 4),
            b"\x1b[<0;5;3m".to_vec()
        );
        assert_eq!(
            mouse(Pointed::ScrollUp, None, 1, 1),
            b"\x1b[<64;2;2M".to_vec()
        );
    }
}
