//! A real terminal, in the rows a tool was given.
//!
//! ```lua
//! casper.tool("htop", {
//!   run    = function() return casper.surface{ rows = 16, about = "htop", tick = 40 } end,
//!   screen = function(args, size) return { command = "htop" } end,
//! })
//! ```
//!
//! **The other kind of tenant.** A `surface` declaration draws its own rows, frame by frame, in
//! Lua. A `screen` declaration names a *program*, and casper puts it on a pty of exactly that
//! size, types into it what the person types, and hands back what it painted. Neither the tool
//! nor the harness draws anything: the program does.
//!
//! Nothing about the wire changes for this. What comes back is the same rows of spans a game
//! sends, so the harness cannot tell `htop` from the dinosaur and does not have to — which is
//! the whole point of the reservation being *space* rather than a widget.
//!
//! **Why casper and not the harness.** Running programs is casper's entire job and the reason it
//! has a spawn link rather than a socket verb. A harness that opened its own pty would be back to
//! spawning commands, which is the thing the split exists to prevent.

use crate::paint::Line;
use crate::tools::{At, Button, Pointed};
use std::io::{Read, Write};
use std::sync::Arc;

pub mod keying;
pub mod noticing;
pub mod painting;
pub mod rewriting;

/// Append a line to `$CASPER_DEBUG_LOG`, if it is set.
///
/// A surface owns its pipes — stdout carries frames and stderr is thrown away by the harness — so
/// printing is not available for diagnosis. This is the only way anything in here can say
/// something to a person. Off unless the variable is set, because a tool that wrote a file nobody
/// asked for is a tool that fills a disk.
fn noted(line: &str) {
    let Some(path) = std::env::var_os("CASPER_DEBUG_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// What to run, as a declaration described it.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    /// The program.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Where to run it.
    pub cwd: Option<String>,
    /// Variables to set on top of the ones casper was started with.
    pub env: Vec<(String, String)>,
}

impl Spec {
    /// Read a spec out of what a `screen` declaration returned, or `None` if it named no program.
    ///
    /// A missing `command` is not a malformed table, it is a declaration that decided there was
    /// nothing to run — so it is a `None` the caller can report rather than a raise.
    #[must_use]
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let command = value.get("command")?.as_str()?.to_owned();
        if command.is_empty() {
            return None;
        }
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|one| one.as_str().map(ToOwned::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let env = value
            .get("env")
            .and_then(serde_json::Value::as_object)
            .map(|table| {
                table
                    .iter()
                    .filter_map(|(key, val)| val.as_str().map(|val| (key.clone(), val.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            command,
            args: strings("args"),
            cwd: value
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .filter(|cwd| !cwd.is_empty())
                .map(ToOwned::to_owned),
            env,
        })
    }
}

/// A program running on a pty, and the screen it has painted so far.
pub struct Screen {
    /// The master side. Shared, because a thread is reading it while this writes to it.
    pty: Arc<pty_process::blocking::Pty>,
    child: std::process::Child,
    /// What the reader thread has picked up and this has not yet fed to the emulator.
    ///
    /// A thread rather than a non-blocking read: the frame loop is synchronous and a read that
    /// blocked would freeze the whole surface until the program next said something, which for
    /// anything waiting on input is forever.
    output: std::sync::mpsc::Receiver<Vec<u8>>,
    vt: vt100::Parser<noticing::Noticing>,
    /// The one dialect difference the emulator does not speak. See [`rewriting`].
    fixing: rewriting::Rewriting,
    named: String,
}

impl Screen {
    /// Put `spec`'s program on a pty `rows` by `cols` and start reading it.
    pub fn open(spec: &Spec, rows: u16, cols: u16) -> anyhow::Result<Self> {
        let (pty, pts) = pty_process::blocking::open()?;
        pty.resize(pty_process::Size::new(rows.max(1), cols.max(1)))?;

        let mut command = pty_process::blocking::Command::new(&spec.command);
        command = command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command = command.current_dir(cwd);
        }
        // **The program is told what it is running on.** Without a `TERM` a curses program either
        // refuses to start or falls back to something from the 1980s, and what it is running on
        // is this emulator — which speaks xterm's sequences and its colours.
        command = command
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LINES", rows.to_string())
            .env("COLUMNS", cols.to_string());
        for (key, value) in &spec.env {
            command = command.env(key, value);
        }
        let child = command.spawn(pts)?;

        let pty = Arc::new(pty);
        let (sender, output) = std::sync::mpsc::channel();
        let reading = Arc::clone(&pty);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            // Ends when the program does: the last slave fd closing makes this read fail, which
            // drops the sender and is how the frame loop learns the program is gone.
            while let Ok(read) = (&*reading).read(&mut buffer) {
                if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        });

        Ok(Self {
            pty,
            child,
            output,
            // With the canary attached: anything the emulator cannot read is counted
            // rather than silently dropped. See [`noticing`].
            vt: vt100::Parser::new_with_callbacks(
                rows.max(1),
                cols.max(1),
                0,
                noticing::Noticing::new(),
            ),
            fixing: rewriting::Rewriting::new(),
            named: spec.command.clone(),
        })
    }

    /// Take everything the program has written since the last frame.
    ///
    /// `false` once it is gone — its output is closed and nothing more will be painted.
    pub fn read(&mut self) -> bool {
        loop {
            match self.output.try_recv() {
                Ok(mut bytes) => {
                    // Before the emulator sees them, because the whole point is that it cannot
                    // read one of these on its own.
                    self.fixing.apply(&mut bytes);
                    self.vt.process(&bytes);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Type a named key into it.
    ///
    /// A key with no byte sequence sends nothing rather than a guess: a byte invented here is a
    /// keystroke the program was never given and cannot be told about.
    pub fn typed(&mut self, name: &str) {
        let application = self.vt.screen().application_cursor();
        if let Some(bytes) = keying::bytes(name, application) {
            let _ = (&*self.pty).write_all(&bytes);
        }
    }

    /// Hand it the pointer, if it asked for one.
    ///
    /// **Only if it asked.** A program that never turned mouse reporting on is one whose input is
    /// a keyboard, and writing escape sequences at it would type garbage into whatever it is
    /// reading — which for a shell is a command somebody did not write.
    pub fn pointed(&mut self, kind: Pointed, button: Option<Button>, row: u16, col: u16) {
        if self.vt.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return;
        }
        let bytes = keying::mouse(kind, button, row, col);
        let _ = (&*self.pty).write_all(&bytes);
    }

    /// Tell it the room changed.
    ///
    /// Both halves, and in this order: the emulator is what the next frame is read out of, and the
    /// `SIGWINCH` the pty sends is what makes the program redraw itself at the new size.
    pub fn resized(&mut self, rows: u16, cols: u16) {
        self.vt.screen_mut().set_size(rows.max(1), cols.max(1));
        let _ = self
            .pty
            .resize(pty_process::Size::new(rows.max(1), cols.max(1)));
    }

    /// What it has painted, and where it left the cursor.
    #[must_use]
    pub fn drawn(&self) -> (Vec<Line>, Option<At>) {
        let screen = self.vt.screen();
        (painting::rows(screen), painting::cursor(screen))
    }

    /// What the model is told once it has ended.
    ///
    /// The status and the screen it left behind, because that is the useful half: a person who ran
    /// a viewer wants the model to know what was on it, and "exited 0" alone says nothing about
    /// what happened. Trimmed of the blank rows a full-screen program pads itself out with.
    #[must_use]
    pub fn epitaph(&mut self) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => status.code().map_or_else(
                || format!("`{}` was killed", self.named),
                |code| format!("`{}` exited with status {code}", self.named),
            ),
            _ => format!("`{}` ended", self.named),
        };
        let left = self.vt.screen().contents();
        let left = left.trim_end();
        if left.is_empty() {
            format!("{status}.")
        } else {
            format!("{status}. What was on the screen:\n\n{left}")
        }
    }

    /// What the emulator could not read, and how often.
    ///
    /// Empty when it understood everything, which is what a screen that came out right looks like
    /// from in here. See [`noticing`].
    #[must_use]
    pub fn dropped(&self) -> Vec<(String, usize)> {
        self.vt.callbacks().dropped()
    }

    /// Ask it to go, and make sure it has.
    pub fn close(&mut self) {
        // **Said on the way out, if anything was lost.** A screen that renders wrong is the one
        // failure here with no clue attached: the program ran, it drew, and what came out is
        // quietly not what it meant. One line naming the sequence turns that into a fact.
        if let Some(said) = self.vt.callbacks().summary() {
            noted(&format!("{}: {said}", self.named));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // A program that outlived the rows it was drawing into would be a `vim` nobody can see and
        // nobody can type at, still holding the file open.
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen running `sh -c script`, given a moment to paint.
    fn ran(script: &str, rows: u16, cols: u16) -> Screen {
        let spec = Spec {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            ..Spec::default()
        };
        let mut screen = Screen::open(&spec, rows, cols).expect("a pty");
        // Polled rather than slept once: a loaded machine takes longer to get a shell started
        // than any single sleep anybody would be willing to write here.
        for _ in 0..200 {
            screen.read();
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

    #[test]
    fn what_the_program_printed_is_on_the_screen() {
        let screen = ran("printf hello", 4, 20);
        let drawn = screen.drawn().0;
        let first: String = drawn[0].iter().map(|span| span.text.as_str()).collect();
        assert!(first.starts_with("hello"), "{first:?}");
    }

    #[test]
    fn the_program_is_given_the_size_it_was_granted() {
        // A curses program lays itself out from this and gets it wrong for the life of the run if
        // the pty was opened at some default and corrected afterwards.
        let screen = ran("stty size", 9, 41);
        let drawn = screen.drawn().0;
        let first: String = drawn[0].iter().map(|span| span.text.as_str()).collect();
        assert_eq!(first.trim(), "9 41", "{first:?}");
    }

    #[test]
    fn a_program_positioning_the_other_way_still_lands_where_it_meant_to() {
        // Through a real pty and the real emulator, because the point of the rewrite is what the
        // emulator does with the byte afterwards — and it is the emulator that drops it.
        for (spelling, name) in [("H", "CUP"), ("f", "HVP")] {
            let screen = ran(&format!(r"printf '\033[3;5{spelling}X'; sleep 2"), 5, 20);
            let rows: Vec<String> = screen
                .drawn()
                .0
                .iter()
                .map(|row| row.iter().map(|span| span.text.as_str()).collect())
                .collect();
            let at: Vec<(usize, usize)> = rows
                .iter()
                .enumerate()
                .filter_map(|(n, row)| row.find('X').map(|col| (n, col)))
                .collect();
            assert_eq!(at, [(2, 4)], "{name}: {rows:?}");
        }
    }

    #[test]
    fn a_program_that_ended_stops_being_read() {
        let mut screen = ran("printf bye", 3, 20);
        for _ in 0..200 {
            if !screen.read() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the program ended and the screen did not notice");
    }

    #[test]
    fn what_it_left_on_the_screen_is_what_the_model_is_told() {
        // The useful half. "exited 0" alone says nothing about what the person just watched.
        let mut screen = ran("printf marker", 3, 20);
        while screen.read() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let said = screen.epitaph();
        assert!(said.contains("marker"), "{said}");
        assert!(said.contains("status 0"), "{said}");
    }

    #[test]
    fn a_declaration_that_names_no_program_opens_nothing() {
        // Rather than a raise. It is a `screen` that decided there was nothing to run, which the
        // caller reports as a tool result the model can read.
        assert!(Spec::from_json(&serde_json::json!({})).is_none());
        assert!(Spec::from_json(&serde_json::json!({"command": ""})).is_none());
    }

    #[test]
    fn a_spec_carries_what_a_declaration_gave_it() {
        let spec = Spec::from_json(&serde_json::json!({
            "command": "htop", "args": ["-d", "10"], "cwd": "/tmp", "env": {"K": "v"},
        }))
        .expect("a program");
        assert_eq!(spec.command, "htop");
        assert_eq!(spec.args, ["-d", "10"]);
        assert_eq!(spec.cwd.as_deref(), Some("/tmp"));
        assert_eq!(spec.env, [("K".to_owned(), "v".to_owned())]);
    }
}
