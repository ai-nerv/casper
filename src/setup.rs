//! Being configured by a coordinator, and by the person at the keyboard.
//!
//! `oslo make install` copies `config/**/*.lua` into `$XDG_CONFIG_HOME/casper`; editing
//! `tools.lua` changes the tools on the next call. Every layer after the shipped one adds to what
//! came before rather than replacing it.

use serde::{Deserialize, Serialize};

/// What sort of value a setting takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Number,
    Flag,
    /// A list or a map, and the description says which.
    Table,
}

/// One thing casper wants to be told.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Need {
    /// What to set, as a configuration names it.
    pub name: String,
    pub kind: Kind,
    /// One line, for a person reading the list.
    pub about: String,
    /// Whether casper cannot work without it.
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// What a coordinator may tell this casper. Every one of them has a default.
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
static TOLD: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();

/// What was set, and what was refused and why.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub set: Vec<String>,
    /// Each with a reason naming the setting.
    pub refused: Vec<Refused>,
}

/// One setting that was not taken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    pub name: String,
    pub why: String,
}

/// Take a configuration chunk; a name nobody declared is refused by name.
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

    // Failure is ignored only because `configure` is the one caller and runs once per process.
    let _ = TOLD.set(serde_json::Value::Object(kept));
    applied
}

/// What a coordinator said, from the environment it spawned this process with.
///
/// casper is one process per call, so whoever spawns it repeats the settings on every spawn, in
/// `CASPER_CONFIGURE`, as the same JSON object `configure` takes.
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
        // Unreadable is empty, not fatal.
        serde_json::from_slice(raw.as_encoded_bytes())
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    })
}

/// One setting a coordinator gave, if it gave one.
#[must_use]
pub fn told(name: &str) -> Option<&'static serde_json::Value> {
    in_force().get(name)
}

/// Where a person's own declarations live: `$XDG_CONFIG_HOME/casper`, else `~/.config/casper`.
#[must_use]
pub fn config_dir() -> Option<std::path::PathBuf> {
    crate::plugins::config_dir()
}

/// Every declarations file to run after the shipped one, in precedence order: the config's own
/// `tools.lua`, then `plugin/` and `pack/`, then `after/`, then a coordinator's `load`.
#[must_use]
pub fn layers() -> Vec<(std::path::PathBuf, crate::plugins::Trust)> {
    crate::plugins::runtimepath(&crate::plugins::Roots {
        config: config_dir(),
        site: crate::plugins::site_dir(),
        // Read here: `plugins` takes its roots as arguments and reads no setting of its own.
        given: match told("load") {
            Some(serde_json::Value::String(named)) => Some(std::path::PathBuf::from(named)),
            _ => None,
        },
    })
}

/// Whether a tool was switched off entirely: not listed, and refused if asked for by name anyway.
#[must_use]
pub fn is_off(tool: &str) -> bool {
    flag(tool, "off")
}

/// Whether a tool is kept runnable but taken out of what the model is shown.
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
#[must_use]
pub fn output_bytes() -> usize {
    const DEFAULT: usize = 262_144;
    told("output_bytes")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT)
}

/// Cut `text` to `output_bytes`, keeping head and tail and cutting on a character boundary.
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

/// Run a coordinator's configuration chunk, as Lua, and say what was done with each name.
///
/// # Errors
/// When the chunk will not run, so that the coordinator is told which part was wrong.
pub fn read(source: &str) -> Result<Applied, String> {
    let mut engine = crate::lua::engine::Engine::new();

    // The engine lends `casper.clients` before any chunk runs; the coordinator's names are the
    // difference against what it already holds.
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
        // `Null` when the value is not describable; a declared setting reads its real value below.
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
        let dir = config_dir().expect("a home or an xdg dir");
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(dir.ends_with("casper"), "{}", dir.display());
    }
}
