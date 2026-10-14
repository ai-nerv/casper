//! The one path a tool call takes, on either door: the declarations loaded fresh, the tool run in
//! this process's jail, what the model reads capped, and one line noted.

use crate::lua::engine::Engine;
use crate::tools::{Call, Ran};

/// An engine with the declarations loaded.
///
/// # Errors
/// When there are no declarations to load.
pub fn loaded() -> Result<Engine, String> {
    let mut engine = Engine::with_configuration(Some(serde_json::json!({
        "web":crate::setup::told("web"),
        "mcp_servers":crate::setup::told("mcp_servers"),
        "tools":crate::setup::told("tools"),
        "mcp_discovery_errors":crate::setup::told("mcp_discovery_errors"),
        "mcp_catalog":crate::setup::told("mcp_catalog"),
    })));

    // The declarations are installed files, not strings in the binary, and the directory is named
    // rather than searched: a relative `config/` would load whichever checkout the working
    // directory happened to be in.
    let known = crate::setup::config_dir()
        .map(|dir| crate::acknowledged::recorded(&crate::acknowledged::manifest_in(&dir)))
        .unwrap_or_default();

    let files = crate::setup::layers();
    if files.is_empty() {
        return Err(format!(
            "no declarations in {}: run `oslo make install` in casper's checkout to put them there",
            crate::setup::config_dir()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "the config directory".to_owned())
        ));
    }

    // The registry replaces by name, so the order is the precedence: `tools.lua`, `plugin/`,
    // installed packages, `after/plugin/`, then a coordinator's own file. A layer that will not
    // run is named on stderr and does not stop the others.
    crate::noted!("setup: {} declaration files", files.len());
    for (path, trust) in files {
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                if trust.needs_acknowledging()
                    && !crate::acknowledged::cleared(&known, &path, &source)
                {
                    crate::noted!("setup: {} held until acknowledged", path.display());
                    aside(format_args!(
                        "casper: {}; run `casper acknowledge` to clear it",
                        crate::acknowledged::Held {
                            path: path.clone(),
                            known: crate::acknowledged::seen(&known, &path),
                        }
                    ));
                    continue;
                }
                if let Err(why) = engine.run(&source, &path.to_string_lossy()) {
                    crate::noted!("setup: {} will not run: {why}", path.display());
                    aside(format_args!("casper: {}: {why}", path.display()));
                }
            }
            Err(why) => {
                crate::noted!("setup: {} cannot be read: {why}", path.display());
                aside(format_args!("casper: {}: {why}", path.display()));
            }
        }
    }

    engine.harvest();
    Ok(engine)
}

/// Say something to a person on stderr without letting a failed write end the process.
fn aside(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = err.write_fmt(args).and_then(|()| err.write_all(b"\n"));
}

/// Run one call, from the door it arrived on.
///
/// # Errors
/// The refusal, when there is no such tool or it is turned off.
pub fn ran(call: &Call) -> Result<Ran, String> {
    let began = std::time::Instant::now();
    let mut engine = loaded()?;
    // Off means gone, not merely unlisted: a model can guess at a tool it was never told about.
    if crate::setup::is_off(&call.tool) {
        crate::noted!("run: {} refused: it is turned off", call.tool);
        return Err(format!("no such tool: {}", call.tool));
    }
    engine.caller(crate::tools::session().as_deref());
    let result = if call.tool.starts_with("mcp__") || call.tool.starts_with("mcp_") {
        Some(crate::mcp_client::call(
            crate::setup::told("mcp_servers"),
            &call.tool,
            &call.args,
        ))
    } else {
        engine.call(&call.tool, &given(call))
    };
    let Some(mut ran) = result else {
        crate::noted!("run: {} refused: no such tool", call.tool);
        return Err(format!("no such tool: {}", call.tool));
    };
    // What the model reads is capped; what the person is shown is not.
    ran.said = crate::setup::bounded(ran.said);
    crate::noted!(
        "run: {} jailed={} → {} in {}ms, {} bytes; brief {} bytes, back {} bytes, keep={}",
        crate::noted::command(&call.tool, arguments(&call.args)),
        crate::jail::Jail::from_env().on(),
        if ran.failed {
            "failed"
        } else if ran.waiting() {
            "asking"
        } else {
            "ok"
        },
        began.elapsed().as_millis(),
        ran.said.len(),
        ran.brief.as_ref().map_or(0, String::len),
        ran.back.as_ref().map_or(0, String::len),
        ran.keep
    );
    Ok(ran)
}

/// How many arguments a tool call carries: the fields of an object, the items of a list.
fn arguments(args: &serde_json::Value) -> usize {
    match args {
        serde_json::Value::Object(fields) => fields.len(),
        serde_json::Value::Array(items) => items.len(),
        serde_json::Value::Null => 0,
        _ => 1,
    }
}

/// What the declaration is handed: the model's arguments, with what the person answered merged
/// in, so a declaration reads `args.answered` to know it is resuming.
fn given(call: &Call) -> serde_json::Value {
    let mut args = call.args.clone();
    let Some(answered) = &call.answered else {
        return args;
    };
    match &mut args {
        serde_json::Value::Object(fields) => {
            fields.insert("answered".to_owned(), serde_json::json!(answered));
        }
        other => *other = serde_json::json!({ "answered": answered }),
    }
    args
}
