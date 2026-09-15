//! Asking the harness something, from inside the frame a tenant is drawing.
//!
//! ```lua
//! local who = casper.knows("session")          -- { id = "…", cwd = "…" }
//! local found = casper.knows("memories", { query = "deploy", limit = 5 })
//! ```
//!
//! A tenant asks in the middle of being asked, and the answer arrives on the same pipe every
//! other frame arrives on. So [`frames`] is the one place frames are read: whatever turned up
//! while somebody was waiting is handed back, in order, before the pipe is read again.

use crate::tools::{FromSurface, ToSurface};
use std::cell::RefCell;
use std::collections::VecDeque;

thread_local! {
    /// Frames that arrived while a tenant was waiting on an answer, oldest first.
    static WAITING: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
    /// The last question asked, so the next one is a different one.
    static ASKED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Whether this process is holding rows, and so has a harness to ask.
    static HOLDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn holding() {
    HOLDING.set(true);
}

/// Every frame the harness sends, in order, until it stops sending them. The one reader: a second
/// would deadlock against this one the first time a tenant asked anything.
pub(crate) fn frames() -> impl Iterator<Item = std::io::Result<String>> {
    std::iter::from_fn(|| {
        if let Some(kept) = WAITING.with_borrow_mut(VecDeque::pop_front) {
            return Some(Ok(kept));
        }
        read_one()
    })
}

/// One line from the harness, or `None` when it has closed the pipe.
fn read_one() -> Option<std::io::Result<String>> {
    use std::io::BufRead;
    let mut line = String::new();
    // Locked and released per line, never held across the loop: a tenant asking a question reads
    // from here too, from inside the frame this lock would still be held for.
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(Ok(line.trim_end().to_owned())),
        Err(why) => Some(Err(why)),
    }
}

/// Put `verb` to the harness and wait for what it says.
///
/// Every path returns; a tenant left waiting holds the rows until the surface times out.
pub(crate) fn wonder(verb: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    // A `run` is one exec whose stdout is its reply, so a question written there would reach the
    // harness as the tool's own result.
    if !HOLDING.get() {
        return Err("casper.knows: only a surface may ask the harness anything".to_owned());
    }
    let wondered = ASKED.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    // A question that did not leave has no answer coming, and the loop below would wait for one.
    if !super::say(&FromSurface::Ask {
        wondered,
        wonder: verb.to_owned(),
        args,
    }) {
        return Err("the question could not be put to the harness".to_owned());
    }

    loop {
        let Some(Ok(line)) = read_one() else {
            return Err("the harness closed while this was being asked".to_owned());
        };
        match super::read(&line) {
            ToSurface::Answer {
                wondered: about,
                answer,
                said,
                because,
            } if about == wondered => {
                return match answer.as_str() {
                    "told" => Ok(said),
                    _ => Err(because),
                };
            }
            // Input the tenant has not seen yet; dropping it would lose a keypress to a question.
            _ => WAITING.with_borrow_mut(|kept| kept.push_back(line)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_that_arrived_while_waiting_is_handed_back_before_the_pipe_is_read() {
        WAITING.with_borrow_mut(|kept| {
            kept.push_back(r#"{"event":"key","key":"j"}"#.to_owned());
            kept.push_back(r#"{"event":"tick"}"#.to_owned());
        });
        let mut frames = frames();
        assert!(matches!(
            frames
                .next()
                .map(|line| super::super::read(&line.expect("a frame"))),
            Some(ToSurface::Key { .. })
        ));
        assert_eq!(
            frames
                .next()
                .map(|line| super::super::read(&line.expect("a frame"))),
            Some(ToSurface::Tick)
        );
    }

    #[test]
    fn nothing_but_a_surface_may_ask() {
        assert!(wonder("session", serde_json::Value::Null).is_err());
    }

    #[test]
    fn what_the_harness_answers_reads_back_as_an_answer() {
        let told = super::super::read(
            r#"{"event":"answer","wondered":3,"answer":"told","said":{"id":"s-7"}}"#,
        );
        let ToSurface::Answer {
            wondered,
            answer,
            said,
            ..
        } = told
        else {
            panic!("the harness told this surface something: {told:?}");
        };
        assert_eq!(wondered, 3);
        assert_eq!(answer, "told");
        assert_eq!(said["id"], "s-7");

        let refused = super::super::read(
            r#"{"event":"answer","wondered":4,"answer":"refused","because":"memories: no balthasar"}"#,
        );
        let ToSurface::Answer {
            answer, because, ..
        } = refused
        else {
            panic!("a refusal is an answer too: {refused:?}");
        };
        assert_eq!(answer, "refused");
        assert!(because.contains("balthasar"), "{because}");
    }

    #[test]
    fn two_questions_are_two_questions() {
        let first = ASKED.with(|n| {
            n.set(n.get() + 1);
            n.get()
        });
        let second = ASKED.with(|n| {
            n.set(n.get() + 1);
            n.get()
        });
        assert_ne!(first, second);
    }
}
