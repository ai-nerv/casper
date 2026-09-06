//! Asking the harness something, from inside the frame a tenant is drawing.
//!
//! ```lua
//! surface = function(args, size)
//!   return function(event)
//!     local who = casper.knows("session")          -- { id = "…", cwd = "…" }
//!     local found = casper.knows("memories", { query = "deploy", limit = 5 })
//!     …
//!   end
//! end,
//! ```
//!
//! **The awkward part is that a tenant asks in the middle of being asked.** The harness sent a
//! key, the tenant is deciding what to draw about it, and half way through that it wants to know
//! what the session remembers. The answer arrives on the same pipe every other frame arrives on,
//! so whatever else was already on its way — a tick, the next keypress — arrives first and must
//! not be thrown away.
//!
//! So there is one place frames are read, and it keeps the ones that turned up while somebody was
//! waiting. [`frames`] hands those back before it reads the pipe again, in the order they came, so
//! a tenant that asks a question loses no input and sees none of it out of order.

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

/// Say that this process is holding rows, and may ask about them.
pub(crate) fn holding() {
    HOLDING.set(true);
}

/// Every frame the harness sends, in order, until it stops sending them.
///
/// The one reader. A second one would take the stdin lock this holds between lines and deadlock
/// the first time a tenant asked anything.
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
    // Locked and released per line rather than held across the loop, because a tenant asking a
    // question reads from here too — and a lock held while its own Lua runs is one it would take
    // again from inside itself.
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(Ok(line.trim_end().to_owned())),
        Err(why) => Some(Err(why)),
    }
}

/// Put `verb` to the harness and wait for what it says.
///
/// `Ok` is what the harness told, `Err` is why it would not — a verb it does not know, a sibling
/// that is not running. Never silence: a tenant left waiting on an answer holds the rows until the
/// whole surface times out, which is a worse failure than being told no.
pub(crate) fn wonder(verb: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    // **Only from inside a surface.** A `run` is one exec whose stdout is its reply, so a
    // question written there would arrive at the harness as the tool's own result — a call that
    // returned a frame instead of an answer, from a tool that looked like it had simply failed.
    if !HOLDING.get() {
        return Err("casper.knows: only a surface may ask the harness anything".to_owned());
    }
    let wondered = ASKED.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    super::say(&FromSurface::Ask {
        wondered,
        wonder: verb.to_owned(),
        args,
    });

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
            // Somebody else's answer, or the next frame of the loop. Kept, because it is input the
            // tenant has not seen yet and dropping it would lose a keypress to a question.
            _ => WAITING.with_borrow_mut(|kept| kept.push_back(line)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_that_arrived_while_waiting_is_handed_back_before_the_pipe_is_read() {
        // The whole point of the queue. A key pressed while a tenant was asking a question is a
        // key the person pressed, and it has to reach them.
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
        // A `run` is one exec whose stdout *is* its reply. A question written there reaches the
        // harness as the tool's own result, and the call looks like it failed.
        assert!(wonder("session", serde_json::Value::Null).is_err());
    }

    #[test]
    fn what_the_harness_answers_reads_back_as_an_answer() {
        // The other half of the wire, written by magi and read here. The two are separate
        // repositories with separate copies of these types, so the shape is checked from a
        // literal rather than from a round trip through one of them.
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
        // An answer names the question it belongs to, so a tenant with more than one in flight can
        // tell them apart. Two that shared a number could not be.
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
