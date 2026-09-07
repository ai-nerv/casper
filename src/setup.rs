//! Being configured by a coordinator, and by the person at the keyboard.
//!
//! **casper was the one program in the family you could not configure.** Its declarations were
//! `include_str!`d, it read no config directory, and it advertised `needs` in its own verb list
//! while dispatching nothing for it — so the program whose entire job is tools was the one you
//! could not add a tool to without a rebuild. That contradicts the principle the family states
//! out loud: nothing is compiled in, *because a binary carrying a copy is a binary you rebuild to
//! fix a wire format*.
//!
//! **The shipped declarations stay, as the floor.** A casper with no configuration is still a
//! casper with thirteen tools; what changes is that there is now somewhere to put a fourteenth.
//! Layering rather than replacing is the family's rule — melchior's `apis.lua` replaced, so
//! adding one wire protocol meant forking eight hundred lines and owning the drift forever, and
//! that is the mistake this copies away from rather than toward.

use serde::{Deserialize, Serialize};

/// What sort of value a setting takes.
///
/// The same four the other siblings declare. Written out here rather than shared, like every
/// other shape that crosses this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A string.
    Text,
    /// A number.
    Number,
    /// True or false.
    Flag,
    /// A table — a list or a map, and the description says which.
    Table,
}

/// One thing casper wants to be told.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Need {
    /// What to set, as a configuration names it.
    pub name: String,
    /// What sort of value it takes.
    pub kind: Kind,
    /// One line, for a person reading the list.
    pub about: String,
    /// Whether casper cannot work without it.
    #[serde(default)]
    pub required: bool,
    /// What it does when nothing is said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// What a coordinator may tell this casper.
///
/// Short, and every one of them has a default. A coordinator that had to fill in a form before
/// starting a tool provider would be a coordinator nobody uses.
#[must_use]
pub fn needs() -> Vec<Need> {
    vec![
        Need {
            name: "tools".to_owned(),
            kind: Kind::Table,
            about: "per tool, by name: `{ dino = { off = true }, shell = { hidden = true } }`. \
                    `off` removes it entirely; `hidden` keeps it runnable and takes it out of \
                    what the model is shown."
                .to_owned(),
            required: false,
            default: None,
        },
        Need {
            name: "load".to_owned(),
            kind: Kind::Text,
            about: "an extra declarations file to run after the shipped ones and after the \
                    config directory"
                .to_owned(),
            required: false,
            default: None,
        },
        Need {
            name: "output_bytes".to_owned(),
            kind: Kind::Number,
            about: "how much of a tool's output crosses back before it is cut".to_owned(),
            required: false,
            default: Some(serde_json::json!(262_144)),
        },
    ]
}

/// What a coordinator said, kept for the life of the process.
///
/// A `OnceLock` rather than a parameter threaded through every call: `configure` arrives on its
/// own invocation, before any tool runs, and the alternative is a field on everything that reads
/// a setting.
static TOLD: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();

/// What was set, and what was refused and why.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    /// Names that were taken.
    pub set: Vec<String>,
    /// Names that were not, each with a reason naming the setting.
    pub refused: Vec<Refused>,
}

/// One setting that was not taken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    /// The name, as it was written.
    pub name: String,
    /// Why not, in terms of what would have been accepted.
    pub why: String,
}

/// Take a chunk of configuration and say what was done with it.
///
/// A name nobody declared is **refused by name**, with what would have been accepted — the same
/// shape melchior and balthasar answer with, because a coordinator should need one reader for the
/// three of them.
#[must_use]
pub fn apply(settings: &serde_json::Map<String, serde_json::Value>) -> Applied {
    let declared = needs();
    let mut applied = Applied::default();
    let mut kept = serde_json::Map::new();

    for (name, value) in settings {
        match declared.iter().find(|need| &need.name == name) {
            Some(_) => {
                applied.set.push(name.clone());
                kept.insert(name.clone(), value.clone());
            }
            None => applied.refused.push(Refused {
                name: name.clone(),
                why: "casper takes no setting by that name; `needs` lists what it takes".to_owned(),
            }),
        }
    }

    let _ = TOLD.set(serde_json::Value::Object(kept));
    applied
}

/// What a coordinator said about `name`, if anything.
#[must_use]
pub fn told(name: &str) -> Option<&'static serde_json::Value> {
    TOLD.get()?.get(name)
}

/// Where a person's own declarations live.
///
/// `$XDG_CONFIG_HOME/casper`, falling back to `~/.config/casper`. Named rather than searched
/// upward: a relative path would load whichever checkout the working directory happened to be
/// in, which is how a sibling ends up running another project's declarations.
#[must_use]
pub fn config_dir() -> Option<std::path::PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(xdg).join("casper"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| std::path::PathBuf::from(home).join(".config/casper"))
}

/// Every declarations file to run, in order, after the shipped one.
///
/// `tools.lua` in the config directory, then anything a coordinator named with `load`. Both are
/// additive: the registry replaces by name, so a file declaring `cat` means it, and a file
/// declaring `mine` adds one.
#[must_use]
pub fn layers() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    if let Some(dir) = config_dir() {
        let theirs = dir.join("tools.lua");
        if theirs.is_file() {
            found.push(theirs);
        }
    }
    if let Some(serde_json::Value::String(named)) = told("load") {
        let path = std::path::PathBuf::from(named);
        if path.is_file() {
            found.push(path);
        }
    }
    found
}

/// Whether a tool was switched off by configuration.
#[must_use]
pub fn is_off(tool: &str) -> bool {
    matches!(
        told("tools")
            .and_then(|tools| tools.get(tool))
            .and_then(|t| t.get("off")),
        Some(serde_json::Value::Bool(true))
    )
}

/// Run a coordinator's configuration chunk and say what was done with each name.
///
/// **Lua on stdin, like every sibling takes it.** A coordinator writes one dialect for the family
/// rather than three, which is the whole reason `configure` is a verb and not a flag.
///
/// Harvested from the VM rather than parsed as JSON: the chunk is a program, and a coordinator
/// that had to serialise its settings into a second format before sending them would be one that
/// could not send a table at all.
///
/// # Errors
/// When the chunk will not run — which is a refusal to report, not a crash: the coordinator sent
/// something, and what it needs back is which part was wrong.
pub fn read(source: &str) -> Result<Applied, String> {
    let mut engine = crate::lua::engine::Engine::new();

    // **What the VM arrives holding is not what the coordinator said.** The engine lends the
    // client libraries as `casper.clients` before any chunk runs, so reading every name after the
    // fact reported one the coordinator never wrote — and refused it, by name, in every reply.
    engine.harvest();
    let before: std::collections::BTreeSet<String> = engine.settings().into_iter().collect();

    engine
        .run(source, "configure")
        .map_err(|why| why.to_string())?;
    engine.harvest();

    let mut settings = serde_json::Map::new();
    for name in engine.settings() {
        if before.contains(&name) {
            continue;
        }
        // `Null` when the value is not describable — the name is what a refusal needs, and a
        // declared setting reads its real value below.
        settings.insert(name, serde_json::Value::Null);
    }
    for need in needs() {
        if let Some(value) = engine.setting(&need.name) {
            settings.insert(need.name.clone(), value);
        }
    }
    Ok(apply(&settings))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn settings(source: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(source).expect("settings")
    }

    #[test]
    fn a_declared_setting_is_taken() {
        let applied = apply(&settings(r#"{"output_bytes": 1024}"#));
        assert_eq!(applied.set, ["output_bytes"]);
        assert!(applied.refused.is_empty());
    }

    #[test]
    fn a_setting_nobody_declared_is_refused_by_name() {
        // The property the family contract asks for: not "that failed", but *which* one and what
        // would have been accepted. A coordinator reading "refused" learns nothing it can act on.
        let applied = apply(&settings(r#"{"nonesuch": 3}"#));
        assert!(applied.set.is_empty());
        assert_eq!(applied.refused.len(), 1);
        assert_eq!(applied.refused[0].name, "nonesuch");
        assert!(
            applied.refused[0]
                .why
                .contains("`needs` lists what it takes"),
            "{}",
            applied.refused[0].why
        );
    }

    #[test]
    fn every_declared_need_has_a_default_or_is_not_required() {
        // A coordinator that had to fill in a form before starting a tool provider is one nobody
        // uses. This is the rule that keeps `needs` from growing into one.
        for need in needs() {
            assert!(
                !need.required || need.default.is_some(),
                "{} is required with no default",
                need.name
            );
        }
    }

    #[test]
    fn the_config_directory_is_named_not_searched() {
        // A relative path would load whichever checkout the working directory happened to be in.
        let dir = config_dir().expect("a home or an xdg dir");
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(dir.ends_with("casper"), "{}", dir.display());
    }
}
