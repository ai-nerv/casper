//! What casper answers, and where.
//!
//! # The shape is the family's
//!
//! ```text
//! -> {"call":"tools","args":[]}
//! <- {"ok":true,"family":1,"n":1,"result":[[{"name":"cat",…}]]}
//! ```
//!
//! Four-byte big-endian length, then the body. `result` is a **list** and `n` says how long it
//! is: a sibling that unpacks a list would read a bare value as *nothing at all*, so `tools()`
//! would come back empty rather than wrong — and an empty answer looks like a casper with no
//! tools. Settled here before either side ships.
//!
//! A refused call is a **reply**, not a dropped connection. The caller sees casper's error
//! rather than a transport error, and "no such call: nope" says what to fix where "connection
//! reset" does not.
//!
//! # Two links, because running a command is not a question
//!
//! casper's whole job is running things, and *a socket that runs commands is remote code
//! execution*. So the surface splits by how much trust the link already carries:
//!
//! | link | verbs | why |
//! |---|---|---|
//! | socket | [`VERBS`] — read-only | anything the walls allow may ask what exists |
//! | spawn (argv + stdin) | `run`, `configure` | the parent could have run the command itself |
//!
//! This is not a precaution that can be added later. A verb that ran something would be reachable
//! by every process of this user the moment it shipped, and taking it away afterwards breaks
//! whoever started calling it.
//!
//! # How this family talks
//!
//! Three transports, two shapes, one encoding. Written out here because it was written out
//! nowhere: four wires had grown four different ways to say the same thing — `say`/`heard`,
//! `to`/`from`, `message`, and a call envelope — and nothing anywhere said which was meant.
//!
//! **Three transports, and the choice between them is about what is being asked.**
//!
//! | | |
//! |---|---|
//! | **argv** | a question with an answer and nothing to hold open. One JSON object on stdout. |
//! | **pipe** | a parent and the child it started. Newline-delimited JSON, both directions. |
//! | **socket** | anything may knock. Four bytes of big-endian length, then JSON. |
//!
//! JSON is on all three. It is the *encoding*, not a transport, and naming it as one is how the
//! diagram of this family came to have "argv + json" on an edge.
//!
//! **Two shapes, and the difference is whether anybody is waiting.**
//!
//! A **call** is answered:
//!
//! ```text
//! -> {"call":"status","args":[]}
//! <- {"ok":true,"family":1,"n":1,"result":[{"busy":false}]}
//! ```
//!
//! An **event** is not:
//!
//! ```text
//! {"event":"listening","at":"…"}
//! ```
//!
//! `result` is a **list** and `n` says how long it is: a sibling that unpacks a list would read
//! a bare value as *nothing at all*, so an answer would come back empty rather than wrong — and
//! an empty answer looks like an empty session. `family` says which revision of this the reply
//! is written in; a reader refuses a number it does not know and tolerates one it predates.
//!
//! A refused call is a **reply**, not a dropped connection. The caller then sees the far end's
//! error rather than a transport error, and "no such call: nope" says what to fix where
//! "connection reset" does not.
//!
//! **The tag key is `event`, everywhere, in both directions.** `scripts/gate-wire.sh` refuses
//! any other, because the failure mode is silent: casper is another checkout with its own copy
//! of these frames, so when two spellings drift nothing fails — the surface simply stops being
//! answered.

use serde::{Deserialize, Serialize};

/// Which revision of the family wire this speaks.
///
/// **There was no version anywhere, in four implementations that already disagree.** casper's
/// reply always sends `n`; a sibling's makes it optional and adds a `fault` field casper has
/// never had. Both are "the family wire". A consumer meeting an unexpected shape learns about it
/// as a missing field at the point of use, which reads as the peer being broken rather than as
/// the peer being a different version.
///
/// The number is duplicated in each sibling for the same reason the types are — a shared crate
/// would be a dependency between repositories, and this family has none. It is bumped when a
/// consumer that does not know about a change would misread a reply, not when a field is added
/// that an older reader ignores.
pub const FAMILY: u16 = 1;

/// The version a reply is stamped with when it does not say.
///
/// Serde needs a function; [`FAMILY`] is the answer.
fn family() -> u16 {
    FAMILY
}

/// One call, as it arrives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Call {
    /// Which verb.
    pub call: String,
    /// Its arguments, in order.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// One reply, as it goes back.
///
/// Built through the constructors rather than by hand, so the `n`/`result` invariant holds in one
/// place instead of at every call site that answers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reply {
    /// Whether the call was answered.
    pub ok: bool,
    /// Which revision of the family wire this reply is written in. See [`FAMILY`].
    ///
    /// Defaulted on the way in, so a reply from a build before this existed reads as `0` — "from
    /// before versions" — rather than failing to parse. A reader refuses a number it does not
    /// know and tolerates one it predates.
    #[serde(default = "family")]
    pub family: u16,
    /// How many values came back. Always `result.len()`.
    #[serde(default)]
    pub n: usize,
    /// The values, in order.
    #[serde(default)]
    pub result: Vec<serde_json::Value>,
    /// Why not, when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    /// An answer of one value.
    #[must_use]
    pub fn of(value: serde_json::Value) -> Self {
        Self {
            ok: true,
            family: FAMILY,
            n: 1,
            result: vec![value],
            error: None,
        }
    }

    /// An answer of none, for a verb that does something rather than reporting something.
    #[must_use]
    pub fn done() -> Self {
        Self {
            ok: true,
            family: FAMILY,
            n: 0,
            result: Vec::new(),
            error: None,
        }
    }

    /// A refusal, which is still a reply.
    #[must_use]
    pub fn refused(why: impl Into<String>) -> Self {
        Self {
            ok: false,
            family: FAMILY,
            n: 0,
            result: Vec::new(),
            error: Some(why.into()),
        }
    }
}

/// The verbs casper answers on its socket.
///
/// Read-only, every one of them. `run` is not here and must never be: see the module docs.
///
/// `verbs` ships from the first version because it cannot be added quietly later — a family where
/// one tool can be asked what it speaks and another cannot has stopped being a family.
pub const VERBS: &[(&str, &str)] = &[
    ("verbs", "what casper answers"),
    (
        "tools",
        "every tool it offers, with schemas and what each needs",
    ),
    ("needs", "what a coordinator may tell it, as declarations"),
];

/// Whether `verb` is one this socket answers.
#[must_use]
pub fn known(verb: &str) -> bool {
    VERBS.iter().any(|(name, _)| *name == verb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_of_one_value_is_a_list_of_one() {
        // The family invariant. A bare value here reads as "returned nothing" to a client that
        // unpacks, and the bug presents as a casper with no tools rather than as an error.
        let wire = serde_json::to_string(&Reply::of(serde_json::json!({"a": 1}))).expect("enc");
        assert_eq!(wire, r#"{"ok":true,"family":1,"n":1,"result":[{"a":1}]}"#);
    }

    #[test]
    fn a_refusal_is_a_reply_rather_than_a_dropped_connection() {
        let wire = serde_json::to_string(&Reply::refused("no such call: nope")).expect("enc");
        assert!(wire.contains(r#""ok":false"#), "{wire}");
        assert!(wire.contains("no such call"), "{wire}");
    }

    #[test]
    fn n_is_always_the_length_of_the_result() {
        for reply in [
            Reply::of(serde_json::Value::Null),
            Reply::done(),
            Reply::refused("no"),
        ] {
            assert_eq!(reply.n, reply.result.len(), "{reply:?}");
        }
    }

    #[test]
    fn the_socket_runs_nothing() {
        // The rule this file exists to hold. A verb that ran something would be reachable by
        // every process of this user the moment it shipped, and taking it away afterwards breaks
        // whoever had started calling it.
        for (verb, _) in VERBS {
            for shape in ["run", "exec", "shell", "eval", "spawn", "call"] {
                assert!(
                    !verb.contains(shape),
                    "`{verb}` is a {shape}-shaped verb on a socket"
                );
            }
        }
    }

    #[test]
    fn verbs_is_answerable_from_the_first_version() {
        assert!(
            known("verbs"),
            "a family tool that cannot say what it speaks"
        );
        assert!(known("tools"));
        assert!(!known("nope"));
    }
}
