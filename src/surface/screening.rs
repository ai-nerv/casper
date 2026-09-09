//! The frame loop for a tenant that is a program rather than a drawing.
//!
//! The same frames in and the same frames out as [`crate::surface`]'s other loop; only who fills
//! the rows differs. A `screen` declaration is asked what to run once, before the program starts,
//! and no Lua is entered per frame after that.

use crate::pty::{Screen, Spec};
use crate::tools::{FromSurface, Held, ToSurface};

/// Run `spec`'s program in `rows` by `cols` until it ends or the reservation does.
pub(super) fn hold<I>(spec: &Spec, rows: u16, cols: u16, frames: I)
where
    I: Iterator<Item = std::io::Result<String>>,
{
    let mut screen = match Screen::open(spec, rows, cols) {
        Ok(screen) => screen,
        // Said rather than silent: the harness is holding rows for this.
        Err(why) => {
            super::say(&FromSurface::Done {
                answered: format!("`{}` would not start: {why}", spec.command),
            });
            return;
        }
    };
    // Drawn before anything is typed, so the rows are filled the moment they appear.
    if !draw(&mut screen) {
        return;
    }

    for line in frames {
        let Ok(line) = line else {
            return;
        };
        match super::read(&line) {
            // A pty has no notion of a key coming back up, so forwarding a release would type
            // every key twice. A repeat is a second keypress and is sent.
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
            // The program is killed rather than left running behind rows nobody can see.
            ToSurface::Close => {
                screen.close();
                return;
            }
            // A pty tenant asks the harness nothing, so an answer here belongs to nobody.
            ToSurface::Open { .. } | ToSurface::Answer { .. } => continue,
        }
        // Read after acting on the frame, so a keystroke and what it produced land in one redraw.
        if !screen.read() {
            // Its output closed: it exited, or it was killed.
            super::say(&FromSurface::Done {
                answered: screen.epitaph(),
            });
            return;
        }
        // A frame the harness did not get ends this too; `Screen`'s own drop closes the program.
        if !draw(&mut screen) {
            return;
        }
    }
}

/// Send whatever the program has painted. `false` when it did not reach the harness.
fn draw(screen: &mut Screen) -> bool {
    let (lines, cursor) = screen.drawn();
    super::say(&FromSurface::Draw { lines, cursor })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen running `script`, read until it has painted something.
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
        // Escape sequences at a program reading a keyboard type garbage into what it is reading.
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
