//! What a tool is, and what running one produced.
//!
//! A tool is a name, a description, a schema, and the permission verb it acts under. casper
//! *describes*; the harness decides. A sibling that could grant itself a permission would make
//! the ledger a suggestion, so nothing here carries an answer to the question a card raises.

use crate::paint::Line;
use serde::{Deserialize, Serialize};

/// One tool, as casper describes it to whoever asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// The name the model calls it by.
    pub name: String,
    /// What it does, in the model's terms.
    pub description: String,
    /// JSON Schema for its arguments.
    pub parameters: serde_json::Value,
    /// The permission verb this tool acts under, if it needs one.
    ///
    /// The harness's own vocabulary — `read`, `write`, `run`, `reach` — because the harness is
    /// what answers. `None` for a tool that touches nothing a person would want a say over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<String>,
}

/// One call, as it arrives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// Which tool.
    pub tool: String,
    /// Its arguments, as the model gave them.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Where the session is rooted, so a relative path means what the person means.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// An answer to the question the last [`Ask`] posed, when this call resumes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<String>,
}

/// What a tool produced.
///
/// Two faces, and either may be absent: a `shell` has a result and no view, a permission question
/// has a view and no result. One field could not hold both without meaning something different
/// each time it was read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ran {
    /// What the model reads.
    ///
    /// Empty for a call that has not finished. Sending the model an empty result would end a
    /// call that is still waiting on a person.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub said: String,
    /// Whether it failed.
    ///
    /// A tool that ran and reported a problem is still a result: the model needs to read what
    /// went wrong in order to do something about it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// What the person sees, when it is more than the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<Shown>,
}

impl Ran {
    /// A result the model reads, with nothing to show beyond it.
    #[must_use]
    pub fn said(text: impl Into<String>) -> Self {
        Self {
            said: text.into(),
            ..Self::default()
        }
    }

    /// A failure the model should read and react to.
    #[must_use]
    pub fn failed(text: impl Into<String>) -> Self {
        Self {
            said: text.into(),
            failed: true,
            ..Self::default()
        }
    }

    /// The same, with a painted view of it.
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
    /// Painted lines, in roles the harness resolves against its palette.
    Painted {
        /// Each line, as the spans it is made of.
        lines: Vec<Line>,
    },
    /// A question for the person, and the answers they may give.
    Ask(Ask),
    /// Rows the tool is asking for, and will fill itself.
    ///
    /// The general form of [`Ask`]. A question has a shape the harness chose; a surface has
    /// whatever shape its tenant draws, and the harness cannot tell a permission prompt from a
    /// file picker from a game. It reserves the rows and blits back what comes out.
    Surface(Surface),
}

/// Rows a tool has asked for, and what to open to fill them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// How many rows it wants. A request, not a grant.
    pub rows: u16,
    /// What this is for, in one line, for a harness that cannot draw it.
    pub about: String,
    /// Milliseconds between ticks, for a surface that moves on its own.
    ///
    /// `None` for one that only answers input — a picker redraws when a key arrives and at no
    /// other time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u16>,
}

/// A question a tool is putting to the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    /// What is being asked, in one line.
    pub question: String,
    /// What may be answered. Never empty: a question with no answers is a message.
    pub options: Vec<Answer>,
    /// More about what is being asked, for the rows under the question.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Line>,
}

/// One answer to an [`Ask`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// What comes back as [`Call::answered`].
    pub id: String,
    /// What the row says.
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
        // What every tool does before anybody writes it a view. A `shell` result should not
        // travel with three nulls describing what it does not have.
        let wire = serde_json::to_string(&Ran::said("a\nb")).expect("encodes");
        assert_eq!(wire, r#"{"said":"a\nb"}"#);
    }

    #[test]
    fn a_question_is_not_a_result_and_says_so() {
        // The distinction the two faces exist for. A reader that took this for a finished call
        // would hand the model an empty string and end a turn that is still waiting on a person.
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
        // Not an error the caller has to invent a message for: whatever went wrong is what the
        // model needs in order to do something about it.
        let ran = Ran::failed("no such file");
        assert!(ran.failed);
        assert_eq!(ran.said, "no such file");
    }

    #[test]
    fn a_card_carries_the_verb_and_no_answer_to_it() {
        // casper describes what a tool would do; the harness decides whether it may. There is
        // nothing here a sibling could set to "allowed".
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

/// What a key did.
///
/// `Down` is the default, so a tenant reading only that behaves the same on a terminal that
/// cannot tell a hold from a tap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Held {
    /// It went down.
    #[default]
    Down,
    /// It is still down, and the terminal is repeating it.
    Repeat,
    /// It came back up.
    Up,
}

/// What the harness sends a surface while it holds its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "to")]
pub enum ToSurface {
    /// The room it actually got, and what the call was given.
    ///
    /// The arguments come with it so a surface opens knowing what it is about — a permission needs
    /// the command, a picker needs the list — rather than being told on some later frame.
    Open {
        /// Rows granted, which may be fewer than were asked for.
        rows: u16,
        /// Columns granted.
        cols: u16,
        /// Whether this terminal reports key repeats and releases.
        ///
        /// `false` without the Kitty keyboard protocol, where every key arrives as a bare press.
        /// A tenant that would wait for a release is told there will never be one.
        #[serde(default)]
        holds: bool,
        /// The call's arguments.
        #[serde(default)]
        args: serde_json::Value,
    },
    /// A key the person pressed while this surface held the rows.
    Key {
        /// `j`, `enter`, `esc`, `ctrl+c`.
        key: String,
        /// Whether it went down, repeated, or came back up.
        ///
        /// Only a terminal speaking the Kitty keyboard protocol can say: without it there is one
        /// indistinguishable press per repeat and no word when a key comes back up. `down` is the
        /// default, and is what every key looks like on a terminal that cannot say more.
        #[serde(default)]
        state: Held,
    },
    /// The room changed under it, because the window did.
    Resize {
        /// Rows now.
        rows: u16,
        /// Columns now.
        cols: u16,
        /// Whether the keyboard reports holds, as currently known.
        ///
        /// Carried here as well as at open because the harness *learns* it: nothing proves the
        /// protocol is live until a repeat or a release actually arrives, which may be long after
        /// this surface opened.
        #[serde(default)]
        holds: bool,
    },
    /// Time passed, for a surface that asked for a tick.
    Tick,
    /// The reservation is over and nothing more will be read.
    Close,
}

/// What a surface sends back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "from")]
pub enum FromSurface {
    /// What to put in the rows, in the same roles everything else is painted in.
    Draw {
        /// Each row, as the spans it is made of.
        lines: Vec<Line>,
    },
    /// The surface is finished, and this is the id of what the person chose.
    ///
    /// An id, never a decision: a surface that returned "allowed" would be a sibling granting
    /// itself a permission. The harness maps this onto its own scopes.
    Done {
        /// The id of whatever was chosen, as the tool named it. Empty when it just ended.
        answered: String,
    },
}
