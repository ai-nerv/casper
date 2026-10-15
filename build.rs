//! The syntaxes `read`, `write` and `edit` highlight with: these languages out of bat's set as
//! two-face ships it, linked into a set of their own and dumped where `src/highlight.rs` reads it.

use serde_json::{Value, json};
use std::error::Error;
use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

/// Kept by bat's name: the languages highlighted, and the hidden syntaxes they reach into.
const KEPT: &[&str] = &[
    "Plain Text",
    "Rust",
    "TOML",
    "Nix",
    "Lua",
    "Zig",
    "Markdown",
    "Go",
    "Python",
    "Regular Expressions (Python)",
    "TypeScript",
    "TypeScriptReact",
    "JavaScript",
    "Regular Expressions (Javascript)",
    "C",
    "C++",
    "Bourne Again Shell (bash)",
    "commands-builtin-shell-bash",
    "JSON",
    "YAML",
];

/// A syntax as the linked set held it: its scope, and its contexts in index order.
struct Linked {
    scope: String,
    contexts: Vec<String>,
}

/// `value` with each linked reference renumbered for the kept set. One into a syntax that was
/// not kept is a reference by scope, which the build falls back to plain text for.
fn relinked(value: &mut Value, kept: &[Option<usize>], linked: &[Linked]) {
    match value {
        Value::Object(fields) => {
            if let Some((syntax, context)) = fields.get("Direct").and_then(id) {
                let target = linked.get(syntax);
                *value = match kept.get(syntax).copied().flatten() {
                    Some(at) => json!({"Direct": {"syntax_index": at, "context_index": context}}),
                    None => json!({"ByScope": {
                        "scope": target.map(|t| t.scope.as_str()),
                        "sub_context": target
                            .and_then(|t| t.contexts.get(context))
                            .filter(|name| *name != "main"),
                        "with_escape": true,
                    }}),
                };
                return;
            }
            if let Some(prototype) = fields.get_mut("prototype")
                && let Some((syntax, context)) = id(prototype)
            {
                *prototype = match kept.get(syntax).copied().flatten() {
                    Some(at) => json!({"syntax_index": at, "context_index": context}),
                    None => Value::Null,
                };
            }
            fields
                .values_mut()
                .for_each(|field| relinked(field, kept, linked));
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| relinked(item, kept, linked)),
        _ => {}
    }
}

fn id(value: &Value) -> Option<(usize, usize)> {
    let index = |key: &str| usize::try_from(value.get(key)?.as_u64()?).ok();
    Some((index("syntax_index")?, index("context_index")?))
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    let all: Vec<SyntaxDefinition> = two_face::syntax::extra_newlines()
        .into_builder()
        .syntaxes()
        .to_vec();
    let linked: Vec<Linked> = all
        .iter()
        .map(|syntax| {
            let mut contexts: Vec<String> = syntax.contexts.keys().cloned().collect();
            contexts.sort();
            Linked {
                scope: syntax.scope.build_string(),
                contexts,
            }
        })
        .collect();
    let mut next = 0;
    let kept: Vec<Option<usize>> = all
        .iter()
        .map(|syntax| {
            KEPT.contains(&syntax.name.as_str()).then(|| {
                next += 1;
                next - 1
            })
        })
        .collect();
    if next != KEPT.len() {
        return Err(format!("two-face has {next} of the {} kept syntaxes", KEPT.len()).into());
    }
    let mut set = SyntaxSetBuilder::new();
    for (syntax, at) in all.iter().zip(&kept) {
        if at.is_some() {
            let mut value = serde_json::to_value(syntax)?;
            relinked(&mut value, &kept, &linked);
            set.add(serde_json::from_value(value)?);
        }
    }
    let set = set.build();
    let unlinked = set.find_unlinked_contexts();
    if !unlinked.is_empty() {
        return Err(format!("unlinked contexts: {unlinked:?}").into());
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("syntaxes.packdump");
    std::fs::write(out, syntect::dumps::dump_binary(&set))?;
    Ok(())
}
