//! What a tool is, and what running one produced. casper describes; the harness decides, so
//! nothing here carries an answer to the question a card raises.

use crate::paint::Line;
use serde::{Deserialize, Serialize};

/// One tool, as casper describes it to whoever asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub name: String,
    pub description: String,
    /// JSON Schema for its arguments.
    pub parameters: serde_json::Value,
    /// The permission verb it acts under, in the harness's vocabulary: `read`, `write`, `run`,
    /// `reach`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<String>,
}

/// One call, as it arrives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// Where the session is rooted, so a relative path means what the person means.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// An answer to the question the last [`Ask`] posed, when this call resumes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<String>,
}

/// What a tool produced: two faces, either of which may be absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ran {
    /// What the model reads. Empty for a call that has not finished.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub said: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<Shown>,
}

impl Ran {
    #[must_use]
    pub fn said(text: impl Into<String>) -> Self {
        Self {
            said: text.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn failed(text: impl Into<String>) -> Self {
        Self {
            said: text.into(),
            failed: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn shown(mut self, lines: Vec<Line>) -> Self {
        self.shown = Some(Shown::Painted { lines });
        self
    }

    /// A question for the person, and no result yet.
    #[must_use]
    pub fn asking(ask: Ask) -> Self {
        Self {
            shown: Some(Shown::Ask(ask)),
            ..Self::default()
        }
    }

    /// Whether this call is waiting on an answer rather than finished.
    #[must_use]
    pub fn waiting(&self) -> bool {
        matches!(self.shown, Some(Shown::Ask(_)))
    }
}

/// What the harness draws for this result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "shown")]
pub enum Shown {
    Painted {
        lines: Vec<Line>,
    },
    Ask(Ask),
    /// Rows the tool reserves and fills itself, in whatever shape its tenant draws.
    Surface(Surface),
}

/// Rows a tool has asked for, and what to open to fill them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// How many rows it wants. A request, not a grant.
    pub rows: u16,
    /// What this is for, in one line, for a harness that cannot draw it.
    pub about: String,
    /// Milliseconds between ticks. `None` for a surface that only redraws when input arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u16>,
}

/// A question a tool is putting to the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub question: String,
    /// What may be answered. Never empty: a question with no answers is a message.
    pub options: Vec<Answer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Line>,
}

/// One answer to an [`Ask`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// What comes back as [`Call::answered`].
    pub id: String,
    pub label: String,
    /// A second line, when the label alone does not say what it means.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::{Role, Span};

    #[test]
    fn a_plain_result_carries_nothing_it_is_not() {
        let wire = serde_json::to_string(&Ran::said("a\nb")).expect("encodes");
        assert_eq!(wire, r#"{"said":"a\nb"}"#);
    }

    #[test]
    fn a_question_is_not_a_result_and_says_so() {
        let ran = Ran::asking(Ask {
            question: "run `rm -rf build`?".to_owned(),
            options: vec![Answer {
                id: "no".to_owned(),
                label: "Deny".to_owned(),
                about: String::new(),
            }],
            detail: Vec::new(),
        });
        assert!(ran.waiting());
        assert!(ran.said.is_empty());
        let wire = serde_json::to_string(&ran).expect("encodes");
        assert!(!wire.contains(r#""said""#), "{wire}");
        assert!(wire.contains(r#""shown":"ask""#), "{wire}");
    }

    #[test]
    fn a_painted_result_is_still_a_result() {
        let ran = Ran::said("-was").shown(crate::paint::diff("-was"));
        assert!(!ran.waiting());
        assert_eq!(ran.said, "-was");
        let wire = serde_json::to_string(&ran).expect("encodes");
        assert!(wire.contains(r#""shown":"painted""#), "{wire}");
        assert!(wire.contains(r#""role":"removed""#), "{wire}");
    }

    #[test]
    fn a_failure_is_a_result_the_model_reads() {
        let ran = Ran::failed("no such file");
        assert!(ran.failed);
        assert_eq!(ran.said, "no such file");
    }

    #[test]
    fn a_card_carries_the_verb_and_no_answer_to_it() {
        let card = Card {
            name: "shell".to_owned(),
            description: "Run a command.".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            needs: Some("run".to_owned()),
        };
        let wire = serde_json::to_string(&card).expect("encodes");
        assert!(!wire.contains("allow") && !wire.contains("grant"), "{wire}");
        assert_eq!(serde_json::from_str::<Card>(&wire).expect("decodes"), card);
    }

    #[test]
    fn a_view_travels_with_its_roles_intact() {
        let ran = Ran::said("fn").shown(vec![vec![Span::new(Role::Keyword, "fn")]]);
        let wire = serde_json::to_string(&ran).expect("encodes");
        let back: Ran = serde_json::from_str(&wire).expect("decodes");
        assert_eq!(back, ran);
    }
}

/// What a key did. `Down` is the default, and is every key on a terminal that cannot say more.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Held {
    #[default]
    Down,
    Repeat,
    Up,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pointed {
    #[default]
    Press,
    Drag,
    Release,
    Moved,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    #[default]
    Left,
    Middle,
    Right,
}

/// A cell, always in the surface's own coordinates: row 0, column 0 is its top-left.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct At {
    pub row: u16,
    pub col: u16,
}

/// What the harness sends a surface while it holds its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum ToSurface {
    /// The room it actually got, and the arguments the call was given.
    Open {
        /// Rows granted, which may be fewer than were asked for.
        rows: u16,
        cols: u16,
        /// Key repeats and releases, reported only under the Kitty keyboard protocol.
        #[serde(default)]
        holds: bool,
        #[serde(default)]
        args: serde_json::Value,
    },
    Key {
        /// `j`, `enter`, `esc`, `ctrl+c`.
        key: String,
        #[serde(default)]
        state: Held,
    },
    Mouse {
        kind: Pointed,
        /// Which button, for the things a button does. Absent for motion and the wheel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<Button>,
        row: u16,
        col: u16,
    },
    Resize {
        rows: u16,
        cols: u16,
        /// Holds as currently known: nothing proves the protocol live until one arrives.
        #[serde(default)]
        holds: bool,
    },
    /// Time passed, for a surface that asked for a tick.
    Tick,
    /// The reservation is over and nothing more will be read.
    Close,
    /// An answer to something this surface asked, out of band from the loop.
    Answer {
        wondered: u64,
        /// `told` or `refused`.
        answer: String,
        /// What the harness said, when it said anything.
        #[serde(default)]
        said: serde_json::Value,
        /// Why it said nothing, when it refused.
        #[serde(default)]
        because: String,
    },
}

/// What a surface sends back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum FromSurface {
    /// What to put in the rows, in the same roles everything else is painted in.
    Draw {
        lines: Vec<Line>,
        /// Where the terminal's own cursor belongs, in this surface's coordinates. `None` leaves
        /// it in the harness's prompt, which is the one an IME and a screen reader follow.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<At>,
    },
    /// The surface is finished. An id, never a decision: the harness maps it onto its own scopes.
    Done {
        /// The id of whatever was chosen, as the tool named it. Empty when it just ended.
        answered: String,
    },
    /// Something this surface asks about the session, from the harness's closed list of verbs.
    Ask {
        /// This question, so its answer can be told from another's.
        wondered: u64,
        wonder: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        args: serde_json::Value,
    },
}
