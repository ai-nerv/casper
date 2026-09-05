//! The frame loop for a tenant that is a *program* rather than a drawing.
//!
//! The same frames in and the same frames out as [`crate::surface`]'s other loop — the harness
//! cannot tell which of the two it is talking to, and that is the point. What is different is
//! only who fills the rows: there, a Lua closure; here, whatever is running on the pty.
//!
//! **Nothing is re-entered per frame.** A `screen` declaration is asked once, before the program
//! starts, what to run; from then on this is Rust talking to a pty. A tenant consulted thirty
//! times a second to be told the same command would be a Lua call per frame for no answer that
//! ever changes.

use crate::pty::{Screen, Spec};
use crate::tools::{FromSurface, Held, ToSurface};

/// Run `spec`'s program in `rows` by `cols` until it ends or the reservation does.
pub(super) fn hold<I>(spec: &Spec, rows: u16, cols: u16, frames: I)
where
    I: Iterator<Item = std::io::Result<String>>,
{
    let mut screen = match Screen::open(spec, rows, cols) {
        Ok(screen) => screen,
        // Said rather than silent. The harness has reserved rows for this and would otherwise
        // hold them for a program that was never going to start.
        Err(why) => {
            super::say(&FromSurface::Done {
                answered: format!("`{}` would not start: {why}", spec.command),
            });
            return;
        }
    };
    // Drawn before anything is typed, so the rows are filled the moment they appear. A program
    // that has not painted yet gives blank ones, which is what it looks like in any terminal.
    draw(&mut screen);

    for line in frames {
        let Ok(line) = line else {
            return;
        };
        match super::read(&line) {
            // **Only going down.** A pty has no notion of a key coming back up: a terminal sends
            // the bytes for a keypress and nothing at all for the release, so forwarding one
            // would type every key twice. A repeat *is* a second keypress and is sent.
            ToSurface::Key { key, state } => {
                if state != Held::Up {
                    screen.typed(&key);
                }
            }
            ToSurface::Mouse {
                kind,
                button,
                row,
                col,
            } => screen.pointed(kind, button, row, col),
            ToSurface::Resize { rows, cols, .. } => screen.resized(rows, cols),
            ToSurface::Tick => {}
            // The reservation is over. The program is killed rather than left running: one that
            // outlived the rows it was drawing into would be an editor nobody can see, nobody can
            // type at, and still holding the file open.
            ToSurface::Close => {
                screen.close();
                return;
            }
            // A pty tenant asks the harness nothing, so an answer here belongs to nobody.
            ToSurface::Open { .. } | ToSurface::Answer { .. } => continue,
        }
        // Read after acting on the frame, so a keystroke and what it produced land in the same
        // redraw rather than a tick apart.
        if !screen.read() {
            // Its output closed: it exited, or it was killed. The rows cannot be filled again.
            super::say(&FromSurface::Done {
                answered: screen.epitaph(),
            });
            return;
        }
        draw(&mut screen);
    }
}

/// Send whatever the program has painted.
fn draw(screen: &mut Screen) {
    let (lines, cursor) = screen.drawn();
    super::say(&FromSurface::Draw { lines, cursor });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames a screen running `script` produces for `sent`.
    ///
    /// Driven the way the harness drives it: an iterator of lines in, and whatever `hold` writes
    /// out — except that here the writes are counted rather than captured, since `say` goes to
    /// this process's stdout. What is asserted is what the *screen* holds.
    fn screen_for(script: &str, rows: u16, cols: u16) -> Screen {
        let spec = Spec {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            ..Spec::default()
        };
        let mut screen = Screen::open(&spec, rows, cols).expect("a pty");
        for _ in 0..200 {
            if !screen.read() {
                break;
            }
            if screen
                .drawn()
                .0
                .iter()
                .any(|row| row.iter().any(|span| !span.text.trim().is_empty()))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        screen
    }

    fn said(screen: &Screen) -> String {
        screen
            .drawn()
            .0
            .iter()
            .map(|row| {
                row.iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn what_is_typed_reaches_the_program() {
        // The half that makes it a screen rather than a picture of one. `cat` echoes what it is
        // given, so what comes back proves the bytes went in.
        let mut screen = screen_for("cat", 3, 20);
        for key in ["h", "i", "enter"] {
            screen.typed(key);
        }
        for _ in 0..200 {
            screen.read();
            if said(&screen).contains("hi") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("nothing was typed into it: {:?}", said(&screen));
    }

    #[test]
    fn a_program_that_never_asked_for_the_mouse_is_not_sent_one() {
        // Writing escape sequences at a program reading a keyboard types garbage into whatever it
        // is reading, which for a shell is a command somebody did not write.
        let mut screen = screen_for("cat", 3, 20);
        screen.pointed(crate::tools::Pointed::Press, None, 1, 1);
        screen.typed("enter");
        for _ in 0..100 {
            screen.read();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!said(&screen).contains('<'), "{:?}", said(&screen));
    }

    #[test]
    fn a_program_that_will_not_start_says_so_rather_than_holding_the_rows() {
        let spec = Spec {
            command: "no-such-program-anywhere".to_owned(),
            ..Spec::default()
        };
        assert!(Screen::open(&spec, 4, 20).is_err());
    }
}
