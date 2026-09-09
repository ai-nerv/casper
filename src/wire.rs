//! What casper answers, and where.
//!
//! ```text
//! -> {"call":"tools","args":[]}
//! <- {"ok":true,"family":1,"n":2,"result":[{"name":"cat",…},{"name":"patch",…}]}
//! ```
//!
//! Newline-delimited JSON on a pipe, one object on stdout for argv. `result` is always a list and
//! `n` its length; a sibling that unpacks a list reads a bare value as nothing at all. A refusal
//! is a reply, not a dropped connection. An event's tag key is `event` in both directions, and
//! `scripts/gate-wire.sh` refuses any other. casper binds no socket, so [`VERBS`] is one list on
//! one door: `run` arrives over the spawn link, where the parent could have run the command
//! itself.

use serde::{Deserialize, Serialize};

/// Which revision of the family wire this speaks. Bumped when a consumer that does not know about
/// a change would misread a reply, not when a field is added that an older reader ignores.
pub const FAMILY: u16 = 1;

/// The revision of the registrar surface a plugin file is written against. It goes up only when
/// something already published stops working. Reported on `verbs`, beside `family`.
pub const SURFACE: u16 = 1;

/// One call, as it arrives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Call {
    pub call: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// Which kind of no an answer is. A refusal costs the caller a feature; a failure costs it the
/// work that just happened.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fault {
    Refused,
    Failed,
}

/// One reply, as it goes back. Built through the constructors, so the `n`/`result` invariant holds
/// in one place rather than at every call site that answers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reply {
    pub ok: bool,
    /// Missing reads as `0`: a peer from before the field, not one that named this revision.
    #[serde(default)]
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
    /// Which kind of no. Set explicitly, so the three siblings refuse in identical bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<Fault>,
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
            fault: None,
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
            fault: None,
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
            fault: None,
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
            fault: Some(Fault::Refused),
        }
    }
}

/// Every verb casper answers. One list because there is one door: casper binds no socket, so a
/// second list would name a door nothing can open.
pub const VERBS: &[(&str, &str)] = &[
    ("verbs", "what this program answers, and on which door"),
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

/// The door every verb above is on.
pub const DOOR: &str = "cli";

#[must_use]
pub fn known(verb: &str) -> bool {
    VERBS.iter().any(|(name, _)| *name == verb)
}
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
    fn no_verb_is_listed_twice() {
        for (verb, _) in VERBS {
            let listed = VERBS.iter().filter(|(name, _)| name == verb).count();
            assert_eq!(listed, 1, "`{verb}` is advertised {listed} times on {DOOR}");
        }
    }

    #[test]
    fn a_fault_is_spelled_the_way_the_family_spells_it() {
        for (fault, spelled) in [
            (Fault::Refused, "\"refused\""),
            (Fault::Failed, "\"failed\""),
        ] {
            let wire = serde_json::to_string(&fault).expect("enc");
            assert_eq!(wire, spelled);
            assert_eq!(
                serde_json::from_str::<Fault>(&wire).expect("dec"),
                fault,
                "a sibling's fault must read back"
            );
        }
    }

    #[test]
    fn no_verb_here_opens_a_door_casper_does_not_have() {
        assert!(
            !known("serve"),
            "`serve` is how this family opens a socket, and `{DOOR}` is stamped on every verb \
             without asking which door it is on — so a socket added here would be advertised as \
             the command line, and the checks that hold casper to one door would all stay green"
        );
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
