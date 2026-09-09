//! What casper answers, and where.
//!
//! ```text
//! -> {"call":"tools","args":[]}
//! <- {"ok":true,"family":1,"n":1,"result":[[{"name":"cat",…}]]}
//! ```
//!
//! Four bytes of big-endian length then JSON on the socket, newline-delimited JSON on a pipe, one
//! object on stdout for argv. `result` is always a list and `n` its length; a sibling that unpacks
//! a list reads a bare value as nothing at all. A refusal is a reply, not a dropped connection. An
//! event's tag key is `event` in both directions, and `scripts/gate-wire.sh` refuses any other.
//! The socket answers [`VERBS`], read-only; `run` and `configure` arrive over the spawn link,
//! where the parent could have run the command itself.

use serde::{Deserialize, Serialize};

/// Which revision of the family wire this speaks. Bumped when a consumer that does not know about
/// a change would misread a reply, not when a field is added that an older reader ignores.
pub const FAMILY: u16 = 1;

/// The revision of the registrar surface a plugin file is written against. It goes up only when
/// something already published stops working. Reported on `verbs`, beside `family`.
pub const SURFACE: u16 = 1;

fn family() -> u16 {
    FAMILY
}

/// One call, as it arrives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Call {
    pub call: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// One reply, as it goes back. Built through the constructors, so the `n`/`result` invariant holds
/// in one place rather than at every call site that answers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reply {
    pub ok: bool,
    /// Defaulted on the way in, so a reply from a build before this existed reads as `0`.
    #[serde(default = "family")]
    pub family: u16,
    /// Only on `verbs`: a fact about the program rather than about the reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<u16>,
    /// Always `result.len()`.
    #[serde(default)]
    pub n: usize,
    #[serde(default)]
    pub result: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    #[must_use]
    pub fn of(value: serde_json::Value) -> Self {
        Self {
            ok: true,
            family: FAMILY,
            surface: None,
            n: 1,
            result: vec![value],
            error: None,
        }
    }

    /// An answer of several values. A list is the rows, not one row that is a list.
    #[must_use]
    pub fn rows(values: Vec<serde_json::Value>) -> Self {
        Self {
            ok: true,
            family: FAMILY,
            surface: None,
            n: values.len(),
            result: values,
            error: None,
        }
    }

    /// An answer of none, for a verb that does something rather than reporting something.
    #[must_use]
    pub fn done() -> Self {
        Self {
            ok: true,
            family: FAMILY,
            surface: None,
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
            surface: None,
            n: 0,
            result: Vec::new(),
            error: Some(why.into()),
        }
    }
}

/// The verbs casper answers on its socket. Read-only, every one of them: `run` is not here and
/// must never be.
pub const VERBS: &[(&str, &str)] = &[
    ("verbs", "what casper answers"),
    (
        "tools",
        "every tool it offers, with schemas and what each needs",
    ),
    ("needs", "what a coordinator may tell it, as declarations"),
];

#[must_use]
pub fn known(verb: &str) -> bool {
    VERBS.iter().any(|(name, _)| *name == verb)
}

/// What the command line answers, as against [`VERBS`], which is the socket. `run` is on this list
/// and deliberately not on the other.
pub const CLI_VERBS: &[(&str, &str)] = &[
    ("verbs", "what this program answers, on each of its doors"),
    (
        "tools",
        "every tool it offers, with schemas and what each needs",
    ),
    ("run", "run one; the call arrives as JSON on stdin"),
    (
        "surface",
        "hold rows on the harness's screen and draw into them",
    ),
    ("needs", "what a coordinator may tell it, as declarations"),
    ("configure", "take that configuration, as Lua on stdin"),
    (
        "client",
        "the client library for its surface — casper has none, and says so",
    ),
    (
        "acknowledge",
        "clear the installed packages, so their declarations may run",
    ),
];
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_of_one_value_is_a_list_of_one() {
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
