//! casper, at a terminal and on a pipe.
//!
//! ```text
//! casper tools          every tool it offers, in the family's reply shape
//! casper run            one call on stdin, one result on stdout
//! casper verbs          what it answers, and on which door
//! ```
//!
//! casper answers on two doors: the command line, and a socket a coordinator binds with `serve`
//! and holds open across a session. What `run` runs on the socket, it runs inside the jail `serve`
//! was spawned with, never one a call named, and only for a call carrying `serve`'s key. See
//! [`casper::serving`].
//!
//! Every verb prints the wire shape, and a refusal is `{"ok":false,…}` with a zero exit. JSON by
//! default, CBOR with `--cbor`: one shape, two encodings.

use casper::tools::{Call, Ran};
use casper::wire::{CLI_VERBS, Reply, SOCKET_VERBS};
mod event_start;

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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["event-capabilities"] {
        return match std::io::Write::write_all(
            &mut std::io::stdout().lock(),
            event_start::CAPABILITIES,
        ) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(_) => std::process::ExitCode::FAILURE,
        };
    }
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
    let _events = match event_start::start(&mut args) {
        Ok(events) => events,
        Err(why) => {
            return if say(how, &Reply::refused(why)) {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            };
        }
    };
    casper::web::crypto();
    let verb = args.first().map_or("help", String::as_str);
    if verb == "mcp" {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(why) => {
                eprintln!("casper {verb}: {why}");
                return std::process::ExitCode::FAILURE;
            }
        };
        let mut term = {
            let _entered = runtime.enter();
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(term) => term,
                Err(why) => {
                    eprintln!("casper {verb}: {why}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        };
        let result = runtime.block_on(async {
            tokio::select! {
                _ = term.recv() => Ok(()),
                result = casper::mcp::serve(&args) => result,
            }
        });
        runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        return match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("casper {verb}: {why}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let began = std::time::Instant::now();
    let delivered = match verb {
        "workflows" => {
            use casper::workflows::protocol::Request;
            let reply = match Request::read(std::io::stdin().lock()) {
                Ok(request) => {
                    let result =
                        if args.len() == 2 && args.get(1).is_some_and(|arg| arg == "--worker") {
                            casper::workflows::protocol::worker(&request)
                        } else if args
                            .iter()
                            .skip(1)
                            .all(|arg| matches!(arg.as_str(), "--json" | "--cbor"))
                        {
                            casper::workflows::invoke(
                                &request,
                                &std::sync::atomic::AtomicBool::new(false),
                            )
                        } else {
                            Err(casper::workflows::Failure::Protocol)
                        };
                    if args.get(1).is_some_and(|arg| arg == "--worker") {
                        return match serde_json::to_writer(
                            std::io::stdout().lock(),
                            &request.response(result),
                        ) {
                            Ok(()) => std::process::ExitCode::SUCCESS,
                            Err(_) => std::process::ExitCode::FAILURE,
                        };
                    }
                    match serde_json::to_value(request.response(result)) {
                        Ok(value) => Reply::of(value),
                        Err(_) => Reply::failed("workflow reply could not be encoded"),
                    }
                }
                Err(why) => Reply::refused(why.to_string()),
            };
            say(how, &reply)
        }
        "verbs" => {
            let mut reply = listing(described());
            reply.surface = Some(casper::wire::SURFACE);
            say(how, &reply)
        }
        "tools" => say(how, &tools()),
        "mcp-auth" | "mcp-status" | "mcp-logout" => {
            let result = casper::calling::loaded()
                .map_err(|_| "MCP configuration unavailable".to_owned())
                .and_then(|_engine| {
                    casper::mcp_client::authentication::command(
                        verb,
                        args.get(1..).unwrap_or_default(),
                        casper::setup::told("mcp_servers"),
                    )
                });
            let reply = match result {
                Ok(value) => Reply::of(value),
                Err(why) => Reply::refused(why),
            };
            say(how, &reply)
        }
        "image" => {
            let reply = args
                .get(1)
                .ok_or_else(|| "image needs a file path".to_owned())
                .and_then(|path| casper::images::jailed_file(path));
            say(
                how,
                &match reply {
                    Ok(image) => Reply::of(serde_json::to_value(image).unwrap_or_default()),
                    Err(why) => Reply::refused(why),
                },
            )
        }
        "code" => {
            if args
                .get(1)
                .is_some_and(|arg| arg == casper::mcp_client::read_broker::FLAG)
            {
                let reply =
                    match casper::mcp_client::read_broker::acquire_input(std::io::stdin().lock()) {
                        Ok(data) => Reply::of(data),
                        Err(why) => Reply::refused(why),
                    };
                return if say(how, &reply) {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            if args
                .get(1)
                .is_some_and(|arg| arg == casper::files::worker::TRANSACT)
            {
                let reply = match casper::files::worker::mutation_input(std::io::stdin().lock()) {
                    Ok(commit) => Reply::of(serde_json::to_value(commit).unwrap_or_default()),
                    Err(why) if why.outcome_unknown() => Reply::failed(why.to_string()),
                    Err(why) => Reply::refused(why.to_string()),
                };
                return if say(how, &reply) {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            if args
                .get(1)
                .is_some_and(|arg| arg == casper::files::worker::FLAG)
            {
                let reply = match casper::files::worker::serve() {
                    Ok(snapshot) => Reply::of(serde_json::to_value(snapshot).unwrap_or_default()),
                    Err(why) => Reply::refused(why.to_string()),
                };
                return if say(how, &reply) {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            if args
                .get(1)
                .is_some_and(|arg| arg == casper::files::worker::OBSERVE)
            {
                let reply = match args
                    .get(2)
                    .ok_or_else(|| "native observation needs a path".to_owned())
                    .and_then(|path| {
                        casper::files::worker::observe(path).map_err(|why| why.to_string())
                    }) {
                    Ok(snapshot) => Reply::of(serde_json::to_value(snapshot).unwrap_or_default()),
                    Err(why) => Reply::refused(why),
                };
                return if say(how, &reply) {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            if args.get(1).is_some_and(|arg| arg == casper::source::FLAG)
                && let Some(path) = args
                    .get(2)
                    .filter(|path| !matches!(path.as_str(), "--json" | "--cbor"))
            {
                let reply = match casper::source::read(path) {
                    Ok(source) => Reply::of(serde_json::to_value(source).unwrap_or_default()),
                    Err(why) => Reply::refused(why.to_string()),
                };
                return if say(how, &reply) {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            let reply = args
                .get(1)
                .ok_or_else(|| "code needs a file or directory path".to_owned())
                .and_then(|path| casper::indexing::path(path));
            say(
                how,
                &match reply {
                    Ok(indexed) => Reply::of(serde_json::to_value(indexed).unwrap_or_default()),
                    Err(why) => Reply::refused(why),
                },
            )
        }
        "grammar" => say(how, &casper::grammars::verb(&args)),
        "run" => {
            if args.iter().any(|arg| arg == "--mcp-read-render") {
                let reply = match casper::mcp::render_input(std::io::stdin().lock()) {
                    Ok(ran) => answer(&ran),
                    Err(why) => Reply::refused(why),
                };
                return if say(how, &reply) {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            if args.iter().any(|arg| arg == "--file-admission") && !file_admission() {
                return std::process::ExitCode::FAILURE;
            }
            say(how, &ran())
        }
        "replace-source" => {
            let reply = match args.get(1) {
                Some(path) => match casper::source::store(path) {
                    Ok(stored) => Reply::of(serde_json::to_value(stored).unwrap_or_default()),
                    Err(casper::source::Error::TooLarge | casper::source::Error::InvalidText) => {
                        Reply::refused(
                            "replacement must be UTF-8 without NUL bytes and smaller than 1 MiB",
                        )
                    }
                    Err(why) => Reply::failed(why.to_string()),
                },
                None => Reply::refused("replace-source needs an existing file path"),
            };
            say(how, &reply)
        }
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
    casper::noted!(
        "cli: {verb} answered in {}ms{}",
        began.elapsed().as_millis(),
        if delivered {
            ""
        } else {
            "; the reply never arrived"
        }
    );
    // Zero for anything casper said, refusals included. Non-zero only when the reply did not
    // reach the caller at all.
    casper::mcp_client::shutdown();
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
    let done = casper::acknowledged::acknowledge(&manifest, &files);
    casper::noted!(
        "acknowledge: {} package files: {}",
        files.len(),
        done.as_ref()
            .map_or_else(Clone::clone, |_| "cleared".to_owned())
    );
    match done {
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
    let needs = casper::setup::needs();
    casper::noted!("needs: {} settings a coordinator may set", needs.len());
    serde_json::to_value(needs).unwrap_or(serde_json::Value::Null)
}

/// Read Lua configuration on stdin, apply it, and say what was done with each name. A chunk that
/// will not run is a refusal rather than a crash.
fn configure() -> Reply {
    let mut source = String::new();
    if let Err(why) = std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut source) {
        return Reply::refused(format!("nothing to read: {why}"));
    }
    let read = casper::setup::read(&source);
    casper::noted!(
        "configure: {}",
        match &read {
            Ok(applied) => casper::noted::short(&serde_json::json!(applied).to_string()),
            Err(why) => format!("refused: {why}"),
        }
    );
    match read {
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
    let engine = match casper::calling::loaded() {
        Ok(engine) => engine,
        Err(why) => return Reply::refused(why),
    };
    let mut available = engine.tools();
    match casper::mcp_client::cards(casper::setup::told("mcp_servers")) {
        Ok(cards) => {
            for card in cards {
                if available.iter().any(|native| native.name == card.name) {
                    return Reply::refused(format!(
                        "MCP tool name collides with a native tool: {}",
                        card.name
                    ));
                }
                available.push(card);
            }
        }
        Err(why) => return Reply::refused(why),
    }
    let mut offered: Vec<_> = available
        .into_iter()
        .filter(|card| !casper::setup::is_off(&card.name) && !casper::setup::is_hidden(&card.name))
        .collect();
    casper::scripting::describe(&mut offered);
    casper::noted!("tools: {} offered", offered.len());
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
    match casper::calling::ran(call, None) {
        Ok(ran) => answer(&ran),
        Err(why) => Reply::refused(why),
    }
}

/// Bind a session's socket and answer `tools` and `run` on it until it closes. The jail every call
/// runs in is this process's own — the one a coordinator gave `serve` at spawn — and a call is
/// answered only with the key the coordinator wrote on `serve`'s standard input.
fn serve(how: As, args: &[String]) -> bool {
    let Some(at) = flag(args, "--at") else {
        return say(
            how,
            &Reply::refused("serve needs --at <path>, under the runtime directory".to_owned()),
        );
    };
    let key = match casper::serving::Key::read(&mut std::io::stdin().lock()) {
        Ok(key) => key,
        Err(why) => return say(how, &Reply::refused(why)),
    };
    let path = std::path::PathBuf::from(&at);
    let bound = match casper::serving::listening_on(&path) {
        Ok(bound) => bound,
        Err(why) => {
            return say(
                how,
                &Reply::refused(format!("serve could not bind {at}: {why}")),
            );
        }
    };
    // Ready before it is used: the coordinator connects once it has read this off stdout.
    listening(&at);
    casper::noted!(
        "serve: listening at {at}, jailed={}",
        casper::jail::Jail::from_env().on()
    );
    if let Err(why) = casper::serving::accept(&bound.listener, key, answer_socket) {
        casper::noted!("serve: {why}");
    }
    true
}

/// One socket call: the tool surface, and a refusal for anything else. `run`'s tool call rides in
/// the first argument, the way the family wraps a verb's payload.
fn answer_socket(call: &casper::wire::Call) -> Reply {
    let began = std::time::Instant::now();
    let reply = on_socket(call);
    casper::noted!(
        "socket: {} {} in {}ms",
        call.call,
        if reply.ok { "answered" } else { "refused" },
        began.elapsed().as_millis()
    );
    reply
}

/// What a socket call is answered with.
fn on_socket(call: &casper::wire::Call) -> Reply {
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
        "{{\"event\":\"listening\",\"at\":{},\"file_admission\":1{}}}\n",
        serde_json::Value::from(at),
        if casper::events::request_context_revision() == Some(1) {
            ",\"request_context\":1"
        } else {
            ""
        }
    );
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
}

fn file_admission() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(b"{\"event\":\"file-admission\",\"revision_contract\":1}\n")
        .and_then(|()| out.flush())
        .is_ok()
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

/// One result, as a reply.
fn answer(ran: &Ran) -> Reply {
    match serde_json::to_value(ran) {
        Ok(value) => Reply::of(value),
        Err(why) => Reply::refused(format!("the result cannot be described: {why}")),
    }
}

/// What a person gets for asking, and whether they got it. Written rather than printed for the
/// reason [`say`] is: `println!` panics on a write that fails.
fn usage() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(
        "casper — the tooling interface\n\
         \n\
         \x20 casper tools        every tool it offers, with schemas\n\
         \x20 casper code PATH    a source file's skeleton, or a directory's entries\n\
         \x20 casper code --source-text PATH   complete UTF-8 text below 1 MiB\n\
         \x20 casper replace-source PATH   replace an existing source from bounded stdin\n\
         \x20 casper grammar fetch|build|list [--dir DIR] [LANG...]\n\
         \x20                     the parsers code reads with, built here as Helix does\n\
         \x20 casper image PATH   read and resize a raster attachment\n\
         \x20 casper mcp          expose configured tools over MCP stdio\n\
         \x20 casper run          one call on stdin, one result on stdout\n\
         \x20 casper serve        bind a session's socket and answer on it\n\
         \x20 casper verbs        what it answers, and on which door\n\
         \n\
         \x20 --json | --cbor   which encoding a reply comes back in\n\
         \n\
         Every verb prints the family's reply shape. casper answers on the\n\
         command line and on a socket it binds with serve. See DESIGN.md.\n"
            .as_bytes(),
    )
    .and_then(|()| out.flush())
    .inspect_err(|why| casper::noted!("usage: it could not be written: {why}"))
    .is_ok()
}

/// Hold a tool's rows, exchanging frames until it is finished. Nothing is printed in the reply
/// shape here: this is a stream of frames, not a call.
fn held(tool: &str) -> bool {
    let Ok(mut engine) = casper::calling::loaded() else {
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
