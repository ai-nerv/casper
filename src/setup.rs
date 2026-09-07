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

/// What a coordinator said, from the environment it spawned this process with.
///
/// **casper is not a daemon, and this is the difference that follows from it.** melchior and
/// balthasar are asked once and then run for the session, so `configure` setting something
/// in-process is the whole of what they need. casper is one process per call: a `configure` that
/// only reached this process would report `set` for a setting that evaporates on exit, which is a
/// program answering the contract and doing nothing.
///
/// So whoever spawns casper says what it should be, on every spawn, in `CASPER_CONFIGURE` — the
/// same JSON object `configure` would have applied. One process, one configuration, no state on
/// disk for two sessions to fight over and none to outlive the session that set it.
///
/// `configure` still exists and still answers, because a coordinator wants to know *which* of its
/// settings would be refused before it commits to them. That is what the verb is for here: a dry
/// run that names what it did not understand.
static FROM_ENV: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();

/// The settings in force: what `configure` set in this process, or what spawned it.
fn in_force() -> &'static serde_json::Value {
    if let Some(told) = TOLD.get() {
        return told;
    }
    FROM_ENV.get_or_init(|| {
        let Some(raw) = std::env::var_os("CASPER_CONFIGURE") else {
            return serde_json::Value::Object(serde_json::Map::new());
        };
        // Unreadable is empty, not fatal. A coordinator that sent something malformed has a bug,
        // and refusing to run any tool over it would take the session down for a setting.
        serde_json::from_slice(raw.as_encoded_bytes())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    })
}

/// One setting a coordinator gave, if it gave one.
#[must_use]
pub fn told(name: &str) -> Option<&'static serde_json::Value> {
    in_force().get(name)
}

/// Where a person's own declarations live.
///
/// `$XDG_CONFIG_HOME/casper`, falling back to `~/.config/casper`. Named rather than searched
/// upward: a relative path would load whichever checkout the working directory happened to be
/// in, which is how a sibling ends up running another project's declarations.
#[must_use]
pub fn config_dir() -> Option<std::path::PathBuf> {
    crate::plugins::config_dir()
}

/// Every declarations file to run, in order, after the shipped one.
///
/// All of it additive: the registry replaces by name, so a file declaring `cat` means it and a
/// file declaring `mine` adds one. The order is the precedence, and it is
/// [`crate::plugins::runtimepath`] — the config's own `tools.lua`, then what is installed under
/// `plugin/` and `pack/`, then `after/`, then whatever a coordinator named with `load`.
///
/// This used to be two files: `tools.lua` and the coordinator's. So the one program in the
/// family whose whole subject is tools could be extended by editing one file or by rebuilding it,
/// and a package somebody else wrote had nowhere to go.
#[must_use]
pub fn layers() -> Vec<(std::path::PathBuf, crate::plugins::Trust)> {
    crate::plugins::runtimepath(&crate::plugins::Roots {
        config: config_dir(),
        site: crate::plugins::site_dir(),
        // The coordinator's file is a *setting*, so it is read here rather than in `plugins`:
        // that module answers "which files, in what order" from what it is handed, and asking it
        // to reach back for a setting would make the two depend on each other in both
        // directions.
        given: match told("load") {
            Some(serde_json::Value::String(named)) => Some(std::path::PathBuf::from(named)),
            _ => None,
        },
    })
}

/// Whether a tool was switched off entirely.
///
/// Off means gone: not listed, and refused if something asks for it by name anyway. A model that
/// was never told about a tool can still guess at one, and answering the guess would make `off`
/// mean "hidden" for anything persistent enough to try.
#[must_use]
pub fn is_off(tool: &str) -> bool {
    flag(tool, "off")
}

/// Whether a tool is kept runnable but taken out of what the model is shown.
///
/// The other half of `off`, and the reason there are two: a tool a *person* invokes through the
/// harness should not be spending context in every request that mentions it.
#[must_use]
pub fn is_hidden(tool: &str) -> bool {
    flag(tool, "hidden")
}

/// One boolean under `tools.<name>`.
fn flag(tool: &str, which: &str) -> bool {
    matches!(
        told("tools")
            .and_then(|tools| tools.get(tool))
            .and_then(|t| t.get(which)),
        Some(serde_json::Value::Bool(true))
    )
}

/// How much of a tool's output crosses back before it is cut.
///
/// **Declared with a default since this existed and applied to nothing.** A setting a program
/// advertises in `needs` and then ignores is worse than one it does not offer: a coordinator sets
/// it, is told it was taken, and the behaviour never changes.
#[must_use]
pub fn output_bytes() -> usize {
    const DEFAULT: usize = 262_144;
    told("output_bytes")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT)
}

/// Cut `text` to `output_bytes`, keeping both ends.
///
/// Head *and* tail, because which one matters depends on the tool: a file read wants its head, a
/// build that failed wants its tail. Cut on a character boundary, so the result is still a string.
#[must_use]
pub fn bounded(text: String) -> String {
    let cap = output_bytes();
    if text.len() <= cap {
        return text;
    }
    let half = cap / 2;
    let head = floor_char_boundary(&text, half);
    let tail = ceil_char_boundary(&text, text.len() - (cap - half));
    let dropped = tail - head;
    format!(
        "{}\n… {dropped} bytes dropped, of {} …\n{}",
        &text[..head],
        text.len(),
        &text[tail..]
    )
}

/// The largest index at or below `at` that starts a character.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The smallest index at or above `at` that starts a character.
fn ceil_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
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
