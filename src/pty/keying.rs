//! Turning a key's *name* back into the bytes a terminal would have sent.
//!
//! The exact inverse of what the harness did to get here. A UI decoded a keypress out of the
//! terminal's escape sequences and passed on the name — see `magi-cli/src/keying.rs` — and a
//! program running on a pty expects those escape sequences and nothing else. So they are built
//! again here, which is the price of a name being the thing that crosses.
//!
//! **Why not pass the bytes through?** Because most tenants are not programs. A picker matching
//! on `"enter"` should not have to know that this terminal sends `\r` and that one might send
//! `\n`, and a name is the only form both a Lua table and a pty can read.
//!
//! Two modes matter, and the screen says which it is in: an application-cursor program wants
//! `ESC O A` for the up arrow where an ordinary one wants `ESC [ A`. Sending the wrong one moves
//! the cursor in `less` and does nothing at all in `vim`'s insert mode.

/// The bytes for `name`, or `None` for a key this cannot express.
///
/// `application` is [`vt100::Screen::application_cursor`]: the program asked for the other arrow
/// encoding, and giving it the wrong one is a key that quietly does nothing.
#[must_use]
pub fn bytes(name: &str, application: bool) -> Option<Vec<u8>> {
    // Modifiers first, outermost in. `alt+` is the escape prefix, which is what a terminal sends
    // for a meta key and what every readline-alike reads.
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
        // Carriage return, not newline. A pty in its usual cooked-adjacent state translates one
        // to the other; sending `\n` to a program in raw mode types a literal linefeed, which in
        // an editor is a character rather than a key.
        "enter" => Some(vec![b'\r']),
        "tab" => Some(vec![b'\t']),
        "backtab" => Some(vec![0x1b, b'[', b'Z']),
        // DEL rather than BS. It is what every terminal on this machine sends, and a program
        // reading BS treats it as ^H, which in `readline` deletes forward.
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
        // The first four are the odd ones: they predate the numbered form and are still what
        // every curses program looks for.
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
        // Anything else is the character itself, which is most of what anybody types.
        other if other.chars().count() == 1 => Some(other.as_bytes().to_vec()),
        _ => None,
    }
}

/// The control code for `ctrl+<something>`.
///
/// The old ASCII arrangement: a letter's low five bits. `ctrl+c` is 3 because `c` is 0x63, and
/// that is the whole rule — there is nothing to look up.
fn control(rest: &str) -> Option<Vec<u8>> {
    let mut chars = rest.chars();
    let (Some(one), None) = (chars.next(), chars.next()) else {
        // `ctrl+enter`, `ctrl+f5` and friends. Real terminals disagree about these and most
        // programs do not read them, so nothing is invented.
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
/// SGR only — `ESC [ < b ; col ; row M|m`. The older encodings put the coordinates in single
/// bytes and cannot say anything past column 223, and every program that reads the mouse at all
/// has understood SGR for a decade.
///
/// Coordinates arrive zero-based, the way the surface counts its own rows, and go out one-based,
/// the way the protocol does.
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
    // Bit 5 is "this is motion", bit 6 is "this is the wheel". A release is the same button with
    // a trailing `m` rather than `M`, which is the whole of how SGR says it.
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
        // Not a newline. A pty translates one to the other; a program in raw mode reading `\n`
        // has been typed a literal linefeed, which in an editor is a character rather than a key.
        assert_eq!(bytes("enter", false), Some(b"\r".to_vec()));
    }

    #[test]
    fn the_arrows_follow_the_mode_the_program_asked_for() {
        // `less` reads the ordinary form and `vim` in insert mode reads the application one.
        // Sending the wrong one is an arrow key that quietly does nothing.
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
        // The first four predate the numbered form and are still what curses looks for.
        assert_eq!(bytes("f1", false), Some(b"\x1bOP".to_vec()));
        assert_eq!(bytes("f5", false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(bytes("f12", false), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn a_key_this_cannot_express_sends_nothing() {
        // Rather than a guess. A byte invented here is a keystroke the program was never given
        // and cannot be told about.
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
        // A release is the same button with a lowercase terminator, which is all SGR does.
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
