//! The one path a tool call takes, on either door and from inside an `execute` script: the
//! declarations loaded fresh, the tool run in this process's jail, what the model reads capped,
//! and one line noted.

use crate::lua::engine::Engine;
use crate::tools::{Call, Card, Ran};
use luna::{Callback, CallbackReturn, Value};

mod files;
mod observing;

/// An engine with the declarations loaded, and `casper.python` lent to them.
///
/// # Errors
/// When there are no declarations to load.
pub fn loaded() -> Result<Engine, String> {
    load(false)
}

fn load(declarations_only: bool) -> Result<Engine, String> {
    let configuration = Some(serde_json::json!({
        "web":crate::setup::told("web"),
        "mcp_servers":crate::setup::told("mcp_servers"),
        "tools":crate::setup::told("tools"),
        "mcp_discovery_errors":crate::setup::told("mcp_discovery_errors"),
        "mcp_catalog":crate::setup::told("mcp_catalog"),
    }));
    let mut engine = if declarations_only {
        Engine::declarations(configuration)
    } else {
        Engine::with_configuration(configuration)
    };
    engine.lend("python", python);

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
                    if declarations_only {
                        return Err(format!(
                            "file revision declarations cannot be admitted: {why}"
                        ));
                    }
                    crate::noted!("setup: {} will not run: {why}", path.display());
                    aside(format_args!("casper: {}: {why}", path.display()));
                }
            }
            Err(why) => {
                if declarations_only {
                    return Err(format!(
                        "file revision declarations cannot be read: {}: {why}",
                        path.display()
                    ));
                }
                crate::noted!("setup: {} cannot be read: {why}", path.display());
                aside(format_args!("casper: {}: {why}", path.display()));
            }
        }
    }

    engine.harvest();
    if !engine.declarations_clean() {
        return Err("file revision declarations attempted an effectful API".into());
    }
    Ok(engine)
}

/// Say something to a person on stderr without letting a failed write end the process.
fn aside(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = err.write_fmt(args).and_then(|()| err.write_all(b"\n"));
}

/// The declared tools a model is shown: neither off nor hidden.
#[must_use]
pub fn offered(engine: &Engine) -> Vec<Card> {
    engine
        .tools()
        .into_iter()
        .filter(|card| !crate::setup::is_off(&card.name) && !crate::setup::is_hidden(&card.name))
        .collect()
}

/// Run one call, from the door it arrived on or from inside the tool named `inside`.
///
/// # Errors
/// The refusal, when there is no such tool or it is turned off.
pub fn ran(call: &Call, inside: Option<&str>) -> Result<Ran, String> {
    let mut trace = observing::Trace::new();
    let result = perform(call, inside, &mut trace);
    trace.finish(&result);
    result
}

fn perform(call: &Call, inside: Option<&str>, trace: &mut observing::Trace) -> Result<Ran, String> {
    let admission = call.revision.validate()?;
    let began = std::time::Instant::now();
    let mut engine = match load(true) {
        Ok(engine) if call.revision.requested() || engine.native(&call.tool).is_some() => engine,
        Err(error) if call.revision.requested() => return Err(error),
        _ => loaded()?,
    };
    // Off means gone, not merely unlisted: a model can guess at a tool it was never told about.
    if crate::setup::is_off(&call.tool) {
        crate::noted!("run: {} refused: it is turned off", call.tool);
        return Err(format!("no such tool: {}", call.tool));
    }
    let native = engine.native(&call.tool);
    if call.revision.requested() && native.is_none() {
        return Err(
            "file revision contract is not supported by this declaration; guarded calls cannot run"
                .into(),
        );
    }
    if let Some(operation) = native {
        if !engine.is_declarations() {
            return Err("file revision declarations could not be admitted without effects".into());
        }
        if admission
            .as_ref()
            .is_some_and(|claimed| claimed.operation != operation)
        {
            return Err("file revision operation changed since capability discovery".into());
        }
    }
    engine.caller(crate::tools::session().as_deref());
    let result = if let Some(operation) = native {
        Some(files::run(call, inside, operation, &mut engine, trace)?)
    } else if call.tool.starts_with("mcp__") || call.tool.starts_with("mcp_") {
        trace.start();
        Some(crate::mcp_client::call(
            crate::setup::told("mcp_servers"),
            &call.tool,
            &call.args,
        ))
    } else {
        if trace.enabled() && engine.declares(&call.tool) {
            trace.start();
        }
        engine.call(&call.tool, &given(call))
    };
    let Some(mut ran) = result else {
        crate::noted!("run: {} refused: no such tool", call.tool);
        return Err(format!("no such tool: {}", call.tool));
    };
    // What the model reads is capped; what the person is shown is not.
    ran.said = crate::setup::bounded(ran.said);
    crate::noted!(
        "run: {}{} jailed={} → {} in {}ms, {} bytes; brief {} bytes, back {} bytes, keep={}",
        crate::noted::command(&call.tool, arguments(&call.args)),
        inside.map_or_else(String::new, |tool| format!(" inside {tool}")),
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

pub(crate) fn brokered_read(call: &Call, data: crate::tools::ReadData) -> Result<Ran, String> {
    let mut trace = observing::Trace::new();
    let result = present_brokered(call, data, &mut trace);
    trace.finish(&result);
    result
}

fn present_brokered(
    call: &Call,
    data: crate::tools::ReadData,
    trace: &mut observing::Trace,
) -> Result<Ran, String> {
    if call.revision.requested() || crate::setup::is_off(&call.tool) {
        return Err("broker data cannot authorize a guarded or disabled tool".into());
    }
    let mut engine = load(true)?;
    if !engine.is_declarations()
        || engine.native(&call.tool) != Some(crate::tools::FileOperation::Read)
    {
        return Err("broker data requires a declaration-only native read".into());
    }
    let mut ran = files::brokered(call, data, &mut engine, trace)?;
    ran.said = crate::setup::bounded(ran.said);
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

/// `casper.python(code, { timeout = n })`: run a script whose tool calls take [`ran`], and answer
/// in the wire's shape.
fn python(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, |ctx, _exec, mut stack| {
        let (code, options): (Value, Value) = stack.consume(ctx)?;
        let code = match code {
            Value::String(code) => String::from_utf8_lossy(code.as_bytes()).into_owned(),
            _ => String::new(),
        };
        let timeout = match options {
            Value::Table(options) => match options.get_value(ctx, "timeout") {
                Value::Integer(seconds) => Some(seconds),
                Value::Number(seconds) => Some(seconds as i64),
                _ => None,
            },
            _ => None,
        };
        let ran = match loaded() {
            Ok(engine) => {
                let cards = offered(&engine);
                let run = |call: &Call| ran(call, Some(crate::scripting::TOOL));
                crate::scripting::run(
                    &code,
                    timeout,
                    &crate::scripting::Host {
                        cards: &cards,
                        run: &run,
                        abandoned: &crate::serving::abandoned,
                    },
                )
            }
            Err(why) => Ran::failed(why),
        };
        let ran = serde_json::to_value(&ran).unwrap_or_else(|why| {
            serde_json::json!({ "said": format!("the result cannot be described: {why}"), "failed": true })
        });
        stack.replace(ctx, crate::lua::convert::lua_from_json(ctx, &ran));
        Ok(CallbackReturn::Return)
    })
}
