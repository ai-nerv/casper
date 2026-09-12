//! casper, at a terminal and on a pipe.
//!
//! ```text
//! casper tools          every tool it offers, in the family's reply shape
//! casper run            one call on stdin, one result on stdout
//! casper verbs          what it answers, and on which door
//! ```
//!
//! casper answers on two doors: the command line, and a socket a coordinator binds with `serve`
//! and holds open across a session. The socket is not a remote shell — what `run` runs, it runs
//! inside the jail `serve` was spawned with, never one a call named. See [`casper::serving`].
//!
//! Every verb prints the wire shape, and a refusal is `{"ok":false,…}` with a zero exit. JSON by
//! default, CBOR with `--cbor`: one shape, two encodings.

use casper::lua::engine::Engine;
use casper::tools::{Call, Ran};
use casper::wire::{CLI_VERBS, Reply, SOCKET_VERBS};

/// Ask the kernel to end this process when whoever started it ends.
///
/// stdin is read to end of file before a tool runs, so from the moment the command starts there
/// is no pipe left to notice a dead caller by. `SIGTERM` rather than `SIGKILL`, so a pty on the
/// far end is hung up rather than left holding a closed screen. The signal watches only from the
/// moment it is set, so the parent pid is read on either side of arming it; comparing against
/// pid 1 instead would break a caller that is itself pid 1 in a container.
fn tie_to_caller() -> Result<(), rustix::io::Errno> {
    let caller = rustix::process::getppid();
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
    if rustix::process::getppid() != caller {
        std::process::exit(0);
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let how = asked(&args);
    if let Err(why) = tie_to_caller() {
        say(
            how,
            &Reply::refused(format!(
                "casper cannot be tied to the process that started it: {why}"
            )),
        );
        return std::process::ExitCode::SUCCESS;
    }
    let delivered = match args.first().map(String::as_str).unwrap_or("help") {
        "verbs" => {
            let mut reply = listing(described());
            reply.surface = Some(casper::wire::SURFACE);
            say(how, &reply)
        }
        "tools" => say(how, &tools()),
        "run" => say(how, &ran()),
        "serve" => serve(how, &args),
        "needs" => say(how, &listing(needs())),
        // A package under `site/pack/` runs once you have said it may, and stops the moment it
        // changes. Your own files run on sight.
        "acknowledge" => say(how, &acknowledged()),
        "configure" => say(how, &configure()),
        "client" | "lua-api" => say(
            how,
            &Reply::refused(
                "casper has no Lua client library: its surface is reached by spawning it with a \
                 call on stdin, or over its socket with the family's length-prefixed framing"
                    .to_owned(),
            ),
        ),
        // Not a call and not a reply: frames both ways for as long as the tool holds its rows.
        "surface" => held(args.get(1).map(String::as_str).unwrap_or_default()),
        "help" | "--help" | "-h" => usage(),
        other => say(how, &Reply::refused(format!("no such call: {other}"))),
    };
    // Zero for anything casper said, refusals included. Non-zero only when the reply did not
    // reach the caller at all.
    if delivered {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// Which encoding the caller asked for. `--json` is accepted and means the default.
fn asked(args: &[String]) -> As {
    if args.iter().any(|a| a == "--cbor") {
        As::Cbor
    } else {
        As::Json
    }
}

/// How a reply leaves.
#[derive(Clone, Copy)]
enum As {
    Json,
    Cbor,
}

/// One reply as bytes, in the encoding the caller asked for. A reply that will not encode as CBOR
/// comes back as JSON rather than as nothing.
fn encoded(how: As, reply: &Reply) -> Vec<u8> {
    if let As::Cbor = how {
        let mut bytes = Vec::new();
        if ciborium::into_writer(reply, &mut bytes).is_ok() {
            return bytes;
        }
    }
    match serde_json::to_string(reply) {
        Ok(line) => format!("{line}\n").into_bytes(),
        Err(why) => {
            format!("{{\"ok\":false,\"family\":1,\"n\":0,\"result\":[],\"error\":\"{why}\"}}\n")
                .into_bytes()
        }
    }
}

/// Print one reply, and say whether the caller got it.
///
/// A refusal still exits zero; what this distinguishes is the reply that never arrived.
fn say(how: As, reply: &Reply) -> bool {
    use std::io::Write;
    // `stdout` is a `LineWriter` and a CBOR frame carries no newline, so an unflushed frame sits
    // in the buffer while `write_all` reports success.
    let mut out = std::io::stdout().lock();
    match out
        .write_all(&encoded(how, reply))
        .and_then(|()| out.flush())
    {
        Ok(()) => true,
        Err(why) => {
            casper::noted!("say: the reply could not be written: {why}");
            false
        }
    }
}

/// Say something to a person on stderr, without letting it end the process: `eprintln!` panics
/// when the write fails, and the panic would cost the reply being assembled.
fn aside(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = err.write_fmt(args).and_then(|()| err.write_all(b"\n"));
}

/// Acknowledge every installed package, so its declarations may run. Nothing installed is an
/// empty answer rather than a refusal.
fn acknowledged() -> Reply {
    let Some(dir) = casper::setup::config_dir() else {
        return Reply::refused("no configuration directory to write a manifest in".to_owned());
    };
    let files: Vec<(std::path::PathBuf, String)> = casper::setup::layers()
        .into_iter()
        .filter(|(_, trust)| trust.needs_acknowledging())
        .filter_map(|(path, _)| {
            std::fs::read_to_string(&path)
                .ok()
                .map(|source| (path, source))
        })
        .collect();

    let manifest = casper::acknowledged::manifest_in(&dir);
    match casper::acknowledged::acknowledge(&manifest, &files) {
        Ok(_) => listing(serde_json::Value::Array(
            files
                .iter()
                .map(|(path, _)| serde_json::json!({ "acknowledged": path.display().to_string() }))
                .collect(),
        )),
        Err(why) => Reply::refused(why),
    }
}
/// A listing verb's answer, as rows: an array becomes the rows, anything else is one row.
fn listing(value: serde_json::Value) -> Reply {
    match value {
        serde_json::Value::Array(rows) => Reply::rows(rows),
        other => Reply::of(other),
    }
}

/// What a coordinator may tell this casper.
fn needs() -> serde_json::Value {
    serde_json::to_value(casper::setup::needs()).unwrap_or(serde_json::Value::Null)
}

/// Read Lua configuration on stdin, apply it, and say what was done with each name. A chunk that
/// will not run is a refusal rather than a crash.
fn configure() -> Reply {
    let mut source = String::new();
    if let Err(why) = std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut source) {
        return Reply::refused(format!("nothing to read: {why}"));
    }
    match casper::setup::read(&source) {
        Ok(applied) => match serde_json::to_value(applied) {
            Ok(value) => Reply::rows(vec![value]),
            Err(why) => Reply::refused(format!("that cannot be described: {why}")),
        },
        Err(why) => Reply::refused(why),
    }
}

/// Every verb, as name, description and the door it is on. A verb on both doors is one row per
/// door, the way the family lists it.
fn described() -> serde_json::Value {
    let cli = CLI_VERBS
        .iter()
        .map(|(name, about)| serde_json::json!({"verb": name, "about": about, "door": "cli"}));
    let socket = SOCKET_VERBS
        .iter()
        .map(|(name, about)| serde_json::json!({"verb": name, "about": about, "door": "socket"}));
    serde_json::Value::Array(cli.chain(socket).collect())
}

/// Every tool, as a card.
fn tools() -> Reply {
    let engine = match loaded() {
        Ok(engine) => engine,
        Err(why) => return Reply::refused(why),
    };
    let offered: Vec<_> = engine
        .tools()
        .into_iter()
        .filter(|card| !casper::setup::is_off(&card.name) && !casper::setup::is_hidden(&card.name))
        .collect();
    match serde_json::to_value(offered) {
        Ok(cards) => listing(cards),
        Err(why) => Reply::refused(format!("the tools cannot be described: {why}")),
    }
}

/// Run one call, read from stdin.
fn ran() -> Reply {
    let mut source = String::new();
    if let Err(why) = std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut source) {
        return Reply::refused(format!("nothing to read: {why}"));
    }
    match serde_json::from_str::<Call>(&source) {
        Ok(call) => run_call(&call),
        Err(why) => Reply::refused(format!("that is not a call: {why}")),
    }
}

/// Run one tool call, wherever it arrived from — stdin on the command line, or a frame on the
/// socket. The jail it runs in is this process's, so a socket call runs inside the walls `serve`
/// was spawned with, never ones the call named.
fn run_call(call: &Call) -> Reply {
    let mut engine = match loaded() {
        Ok(engine) => engine,
        Err(why) => return Reply::refused(why),
    };
    // Off means gone, not merely unlisted: a model can guess at a tool it was never told about.
    // `hidden` is the setting that means hidden, and it still runs.
    if casper::setup::is_off(&call.tool) {
        return Reply::refused(format!("no such tool: {}", call.tool));
    }
    let Some(mut ran) = engine.call(&call.tool, &given(call)) else {
        return Reply::refused(format!("no such tool: {}", call.tool));
    };
    // What the model reads is capped; what the person is shown is not.
    ran.said = casper::setup::bounded(ran.said);
    answer(&ran)
}

/// Bind a session's socket and answer `tools` and `run` on it until it closes. The jail every call
/// runs in is this process's own — the one a coordinator gave `serve` at spawn — so the socket runs
/// only what that jail allows, which is why it is a tool surface and not a remote shell.
fn serve(how: As, args: &[String]) -> bool {
    let Some(at) = flag(args, "--at") else {
        return say(
            how,
            &Reply::refused("serve needs --at <path>, under the runtime directory".to_owned()),
        );
    };
    let path = std::path::PathBuf::from(&at);
    let listener = match casper::serving::listening_on(&path) {
        Ok(listener) => listener,
        Err(why) => {
            return say(
                how,
                &Reply::refused(format!("serve could not bind {at}: {why}")),
            );
        }
    };
    // Ready before it is used: the coordinator connects once it has read this off stdout.
    listening(&at);
    if let Err(why) = casper::serving::accept(&listener, answer_socket) {
        casper::noted!("serve: {why}");
    }
    true
}

/// One socket call: the tool surface, and a refusal for anything else. `run`'s tool call rides in
/// the first argument, the way the family wraps a verb's payload.
fn answer_socket(call: &casper::wire::Call) -> Reply {
    match call.call.as_str() {
        "tools" => tools(),
        "run" => match call.args.first() {
            Some(value) => match serde_json::from_value::<Call>(value.clone()) {
                Ok(one) => run_call(&one),
                Err(why) => Reply::refused(format!("that is not a call: {why}")),
            },
            None => Reply::refused("run takes one call as its argument".to_owned()),
        },
        other => Reply::refused(format!("no such call on the socket: {other}")),
    }
}

/// Tell the coordinator, on stdout, that the socket is bound and ready to be connected to.
fn listening(at: &str) {
    use std::io::Write;
    let line = format!(
        "{{\"event\":\"listening\",\"at\":{}}}\n",
        serde_json::Value::from(at)
    );
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
}

/// One flag's value, as `--name value` or `--name=value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == name {
            return rest.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_owned());
        }
    }
    None
}

/// What the declaration is handed: the model's arguments, and what the person answered.
///
/// Merged rather than nested, so a declaration reads `args.answered` to know it is resuming.
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

/// One result, as a reply.
fn answer(ran: &Ran) -> Reply {
    match serde_json::to_value(ran) {
        Ok(value) => Reply::of(value),
        Err(why) => Reply::refused(format!("the result cannot be described: {why}")),
    }
}

/// An engine with the declarations loaded.
fn loaded() -> Result<Engine, String> {
    let mut engine = Engine::new();

    // The declarations are installed files, not strings in the binary, and the directory is named
    // rather than searched: a relative `config/` would load whichever checkout the working
    // directory happened to be in.
    let known = casper::setup::config_dir()
        .map(|dir| casper::acknowledged::recorded(&casper::acknowledged::manifest_in(&dir)))
        .unwrap_or_default();

    let files = casper::setup::layers();
    if files.is_empty() {
        return Err(format!(
            "no declarations in {}: run `oslo make install` in casper's checkout to put them there",
            casper::setup::config_dir()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "the config directory".to_owned())
        ));
    }

    // The registry replaces by name, so the order is the precedence: `tools.lua`, `plugin/`,
    // installed packages, `after/plugin/`, then a coordinator's own file. A layer that will not
    // run is named on stderr and does not stop the others.
    for (path, trust) in files {
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                if trust.needs_acknowledging()
                    && !casper::acknowledged::cleared(&known, &path, &source)
                {
                    aside(format_args!(
                        "casper: {}; run `casper acknowledge` to clear it",
                        casper::acknowledged::Held {
                            path: path.clone(),
                            known: casper::acknowledged::seen(&known, &path),
                        }
                    ));
                    continue;
                }
                if let Err(why) = engine.run(&source, &path.to_string_lossy()) {
                    aside(format_args!("casper: {}: {why}", path.display()));
                }
            }
            Err(why) => aside(format_args!("casper: {}: {why}", path.display())),
        }
    }

    engine.harvest();
    Ok(engine)
}

/// What a person gets for asking, and whether they got it. Written rather than printed for the
/// reason [`say`] is: `println!` panics on a write that fails.
fn usage() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(
        concat!(
            "casper — the tooling interface\n\
         \n\
         \x20 casper tools        every tool it offers, with schemas\n\
         \x20 casper run          one call on stdin, one result on stdout\n\
         \x20 casper serve        bind a session's socket and answer on it\n\
         \x20 casper verbs        what it answers, and on which door\n\
         \n\
         \x20 --json | --cbor   which encoding a reply comes back in\n\
         \n\
         Every verb prints the family's reply shape. casper answers on the\n\
         command line and on a socket it binds with serve. See DESIGN.md.\n"
        )
        .as_bytes(),
    )
    .and_then(|()| out.flush())
    .inspect_err(|why| casper::noted!("usage: it could not be written: {why}"))
    .is_ok()
}

/// Hold a tool's rows, exchanging frames until it is finished. Nothing is printed in the reply
/// shape here: this is a stream of frames, not a call.
fn held(tool: &str) -> bool {
    let Ok(mut engine) = loaded() else {
        return casper::surface::nothing_to_draw();
    };
    casper::surface::hold(tool, &mut engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_the_default_and_cbor_is_asked_for() {
        assert!(matches!(asked(&[]), As::Json));
        assert!(matches!(asked(&["tools".to_owned()]), As::Json));
        assert!(matches!(
            asked(&["tools".to_owned(), "--json".to_owned()]),
            As::Json
        ));
        assert!(matches!(
            asked(&["tools".to_owned(), "--cbor".to_owned()]),
            As::Cbor
        ));
    }

    #[test]
    fn both_encodings_carry_the_same_answer() {
        let reply = Reply::of(serde_json::json!([{ "name": "cat" }]));
        let from_json: serde_json::Value =
            serde_json::from_slice(&encoded(As::Json, &reply)).expect("json");
        let from_cbor: serde_json::Value =
            ciborium::from_reader(encoded(As::Cbor, &reply).as_slice()).expect("cbor");
        assert_eq!(from_json, from_cbor, "one shape, two encodings");
        assert_eq!(
            from_cbor["family"],
            serde_json::json!(1),
            "and it says which"
        );
    }

    #[test]
    fn a_refusal_encodes_in_both_too() {
        // The JSON fallback in `encoded` is hand-written, so it needs exercising.
        let reply = Reply::refused("no such tool: nope".to_owned());
        let from_cbor: serde_json::Value =
            ciborium::from_reader(encoded(As::Cbor, &reply).as_slice()).expect("cbor");
        assert_eq!(from_cbor["ok"], serde_json::json!(false));
    }
    #[test]
    fn a_listing_is_rows_and_not_one_row_that_is_a_list() {
        let reply = listing(serde_json::json!([{ "name": "tools" }, { "name": "load" }]));
        assert_eq!(reply.n, 2, "n is the number of rows");
        assert_eq!(reply.result.len(), 2);
        assert!(reply.result[0].is_object(), "a row is not a list");
    }

    /// Every verb is on a door casper can open, and no verb twice on one door. A verb may be on
    /// both — `tools` and `run` are.
    #[test]
    fn every_verb_is_advertised_on_a_door_casper_opens() {
        let rows = listing(described()).result;
        let mut seen: Vec<(String, String)> = Vec::new();
        for row in &rows {
            let door = row["door"].as_str().unwrap_or_default().to_owned();
            assert!(
                door == "cli" || door == "socket",
                "casper opens only these doors: {row}"
            );
            let verb = row["verb"].as_str().unwrap_or_default().to_owned();
            let pair = (verb, door);
            assert!(
                !seen.contains(&pair),
                "`{pair:?}` is advertised twice on its door"
            );
            seen.push(pair);
        }
        // A socket door with no `serve` on the command line is one nothing can open.
        assert!(
            rows.iter()
                .any(|row| row["door"] == "socket" && row["verb"] == "run"),
            "the socket carries the tool surface: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row["door"] == "cli" && row["verb"] == "serve"),
            "a socket door needs serve to open it: {rows:?}"
        );
    }

    #[test]
    fn n_is_the_number_of_rows_for_every_verb_that_lists() {
        for reply in [listing(described()), listing(needs())] {
            assert_eq!(reply.n, reply.result.len(), "{reply:?}");
            assert!(reply.n > 1, "a listing has rows: {reply:?}");
            for row in &reply.result {
                assert!(row.is_object(), "{row}");
            }
        }
    }
}
