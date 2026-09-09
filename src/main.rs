//! casper, at a terminal and on a pipe.
//!
//! ```text
//! casper tools          every tool it offers, in the family's reply shape
//! casper run            one call on stdin, one result on stdout
//! casper verbs          what its socket answers
//! ```
//!
//! **`run` is here and not on the socket.** casper's job is running programs, and a socket that
//! runs commands is a remote shell wearing a friendly name. The spawn link carries the trust
//! instead: a parent that can spawn casper could have run the command itself, so nothing is
//! granted by handing it over. See [`casper::wire`].
//!
//! Every verb prints the **wire** shape, not something shaped for a person to read, and a refusal
//! is `{"ok":false,…}` with a zero exit. Otherwise every client needs two parsers and a real
//! error arrives as "exited 1".
//!
//! **In JSON or in CBOR**, chosen with `--json` or `--cbor`. One shape, two encodings: the family
//! settled on JSON as what a reply *is*, and CBOR is the same reply for a caller that is not
//! going to read it. The other two siblings took the pair on their one-shot doors and this one
//! did not, so a caller asking the family for CBOR still had to keep a JSON parser for casper.

use casper::lua::engine::Engine;
use casper::tools::{Call, Ran};
use casper::wire::{Reply, VERBS};

/// Ask the kernel to end this process when whoever started it ends.
///
/// **casper is nobody's daemon.** It is one exec per call, started by a magi and belonging to
/// it, and every way it has of noticing that magi has gone runs out before the work does: the
/// call arrives on stdin and stdin is read to end of file *before* a tool is run, so from the
/// moment the command starts there is no pipe left to close. A magi killed in the middle of a
/// call left casper reparented to init with nothing to answer to. `PR_SET_PDEATHSIG` needs no
/// pipe: a `kill -9`, an OOM and a panic that runs no destructor are covered exactly as well as
/// a clean exit is.
///
/// `SIGTERM` rather than `SIGKILL`. Both end this, and the first lets the pty go the way a
/// hangup does, so the program on the far end is told rather than left holding a closed screen.
///
/// The signal watches only from the moment it is set, so a caller that died a moment before is
/// a death nothing was ever sent for. Reading who the parent is on either side of the call
/// closes what can be closed from in here: a different answer means the reparenting has already
/// happened. The window before the first of those reads would take the caller naming its own
/// pid, the way balthasar's `--tied` does, and it does not have to: a call that never arrives
/// is refused a moment later anyway. Comparing against pid 1 is the tempting version and is
/// wrong — a caller that is itself pid 1 in a container would spawn a casper that answers
/// nothing.
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
    // Refused in the reply shape rather than on stderr with an exit code, like every other
    // refusal here: a client that has to parse two things parses neither.
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
        // The one verb that says which registrar surface this program offers -- a fact about the
        // program, not about the reply, so it rides on the self-description and nowhere else.
        "verbs" => {
            let mut reply = listing(described());
            reply.surface = Some(casper::wire::SURFACE);
            say(how, &reply)
        }
        "tools" => say(how, &tools()),
        "run" => say(how, &ran()),
        // The coordinated half of the family contract. casper listed `needs` in its own verb
        // table and dispatched nothing for it, so the answer to "what may I tell you" was
        // "no such call" — from the one program in the family whose whole subject is tools.
        "needs" => say(how, &listing(needs())),
        // The distribution half. A package under `site/pack/` is somebody else's code that
        // arrived by being fetched, so it runs once you have said it may -- and stops the moment
        // it changes. Your own files are yours and run on sight.
        "acknowledge" => say(how, &acknowledged()),
        "configure" => say(how, &configure()),
        // `client` is the family's name for it. casper's surface is reached by spawning it with
        // a JSON call, so there is no Lua library to hand over — and that is an answer, where
        // saying nothing is not.
        "client" | "lua-api" => say(
            how,
            &Reply::refused(
                "casper has no client library: its surface is reached by spawning it with a call \
                 on stdin, not from a Lua VM"
                    .to_owned(),
            ),
        ),
        // Not a call and not a reply: frames both ways for as long as the tool holds its rows.
        // See `casper::surface` for why this cannot be one exec per event. The exit code still
        // says whether what casper wrote reached whoever asked.
        "surface" => held(args.get(1).map(String::as_str).unwrap_or_default()),
        "help" | "--help" | "-h" => usage(),
        other => say(how, &Reply::refused(format!("no such call: {other}"))),
    };
    // Zero for anything casper *said*, refusals included — that is the family's rule and clients
    // are written to it. Non-zero only when the reply did not reach the caller at all, which is
    // not a refusal and not an answer, and which nothing else can report.
    if delivered {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// Which encoding the caller asked for.
///
/// JSON unless CBOR was named, which is the family's rule — melchior and balthasar take the same
/// pair of flags on their own one-shot doors. `--json` is accepted and means the default, so a
/// caller can be explicit without having to know which sibling treats it as which.
///
/// Scanned from argv rather than parsed: the verb is the first word and everything a tool needs
/// arrives on stdin, so there is no argument this can be confused with.
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
    /// Text, and the default.
    Json,
    /// Bytes, for a caller that is not going to read it.
    Cbor,
}

/// One reply as bytes, in the encoding the caller asked for.
///
/// A reply that will not encode as CBOR comes back as JSON rather than as nothing. Silence is the
/// one answer a client cannot read — it waits for a frame that never comes — so every path here
/// ends in bytes.
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

/// Print one reply, in the shape every client parses and the encoding it asked for.
///
/// **Answers whether the caller actually got it.** This was a `let _ =` on the write, which is
/// the one place in casper where swallowing an error costs the caller everything: the reply *is*
/// the answer, and a short write or a closed pipe left casper exiting 0 having said nothing or
/// half of something. A truncated JSON frame reads at the far end as casper being broken, and
/// there is no second channel to say otherwise — stderr is thrown away by the harness.
///
/// A refusal still exits zero, and that stays true: a refusal is an answer. What this
/// distinguishes is the reply that never arrived, which is the one case an exit code is the only
/// thing left to carry.
fn say(how: As, reply: &Reply) -> bool {
    use std::io::Write;
    // Flushed here: `stdout` is a `LineWriter`, and a CBOR frame carries no newline to push it
    // out, so the failure would otherwise surface only in the drop that ignores it.
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

/// Say something to a person on stderr, without letting it end the process.
///
/// **`eprintln!` panics when the write fails, and the panic costs the reply.** A broken
/// declaration is named here while the answer is still being assembled, so a casper run with its
/// stderr on a full device or a closed pipe exited 101 with an empty stdout — the diagnostic
/// destroyed the thing it was diagnosing. Nothing reads stderr: the harness throws it away, so a
/// note that cannot be delivered is dropped rather than raised.
fn aside(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = err.write_fmt(args).and_then(|()| err.write_all(b"\n"));
}

/// Acknowledge every installed package, so its declarations may run.
///
/// Answers with what it took, in the family's reply shape like everything else here. Nothing
/// installed is an empty answer rather than a refusal: it is what a machine that has installed
/// nothing should say.
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
/// A listing verb's answer, as rows.
///
/// An array becomes the rows themselves. Anything else is one row, which is what it already was.
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

/// Read configuration on stdin, apply it, and say what was done with each name.
///
/// Lua on stdin, like every other sibling takes it — the coordinator writes one dialect, not
/// three. A chunk that will not run is a refusal rather than a crash: the coordinator sent
/// something, and what it needs back is which part was wrong.
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

/// What the socket answers, as name and description.
fn described() -> serde_json::Value {
    // Both doors, each verb saying which it is on. A conformance check that probed the socket
    // list against the command line would report `run` as missing from a door that never
    // claimed it — and miss that `needs` was claimed by a door that did not answer it.
    let cli = casper::wire::CLI_VERBS
        .iter()
        .map(|(name, about)| serde_json::json!({"verb": name, "about": about, "door": "cli"}));
    let socket = VERBS
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
    // **What a coordinator switched off is not listed.** `off` and `hidden` were declared in
    // `needs` from the day `needs` existed and were read by nothing at all: a coordinator set
    // one, was told it was taken, and every tool stayed exactly where it was.
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
    let call: Call = match serde_json::from_str(&source) {
        Ok(call) => call,
        Err(why) => return Reply::refused(format!("that is not a call: {why}")),
    };
    let mut engine = match loaded() {
        Ok(engine) => engine,
        Err(why) => return Reply::refused(why),
    };
    // **Off means gone, not merely unlisted.** A model that was never told about a tool can
    // still guess at one, and answering the guess would make `off` mean `hidden` for anything
    // persistent enough to try. `hidden` is the setting that means hidden, and it still runs.
    if casper::setup::is_off(&call.tool) {
        return Reply::refused(format!("no such tool: {}", call.tool));
    }
    // A tool nobody declared is a refusal rather than a failed result: the model asked for
    // something that does not exist, and telling it the call *failed* invites a retry.
    let Some(mut ran) = engine.call(&call.tool, &given(&call)) else {
        return Reply::refused(format!("no such tool: {}", call.tool));
    };
    // What the model reads is capped; what the person is shown is not, because it is drawn once
    // and costs no context.
    ran.said = casper::setup::bounded(ran.said);
    answer(&ran)
}

/// What the declaration is handed: the model's arguments, and what the person answered.
///
/// **The answer travels with the arguments.** A declaration reads `args.answered` to know it is
/// resuming, and passing the arguments alone meant it never saw one — so a tool that asked
/// asked again on every call, forever, and the caller gave up on it rather than the person
/// giving up on the question.
///
/// Merged rather than nested so a declaration writes `args.answered` and not
/// `args.call.answered`: the answer is one more thing known about this call, which is what an
/// argument is.
fn given(call: &Call) -> serde_json::Value {
    let mut args = call.args.clone();
    let Some(answered) = &call.answered else {
        return args;
    };
    match &mut args {
        serde_json::Value::Object(fields) => {
            fields.insert("answered".to_owned(), serde_json::json!(answered));
        }
        // A tool taking no arguments still has to be able to be resumed, and there is nothing
        // to merge into — so the answer becomes the whole of what it is given.
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

    // **The declarations are a file, not a string in the binary.** They were `include_str!`d, so
    // changing one tool -- or reading what the thirteen actually do -- meant a rebuild, and the
    // config directory could only ever layer over something a person could not see or edit.
    // melchior and balthasar have installed their declarations since they had any; casper was the
    // one that did not, and there was no reason for it to be different.
    //
    // Named rather than searched: `$XDG_CONFIG_HOME/casper`, because a relative `config/` would
    // load whichever checkout the working directory happened to be in -- which is how a sibling
    // ends up running another project's tools.
    let known = casper::setup::config_dir()
        .map(|dir| casper::acknowledged::recorded(&casper::acknowledged::manifest_in(&dir)))
        .unwrap_or_default();

    let files = casper::setup::layers();
    if files.is_empty() {
        // A casper with no declarations is not a casper, and saying so beats answering an empty
        // tool list -- which reads to a coordinator as "this machine has no tools" rather than
        // as "this install is half finished".
        return Err(format!(
            "no declarations in {}: run `oslo make install` in casper's checkout to put them there",
            casper::setup::config_dir()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "the config directory".to_owned())
        ));
    }

    // Layered, because the registry replaces by name: a file declaring `cat` means it, and a file
    // declaring something new adds one. The order is the precedence -- `tools.lua`, then
    // `plugin/`, then installed packages, then `after/plugin/`, then a coordinator's own file.
    //
    // A layer that will not run is named on stderr and does not stop the others: a broken file of
    // somebody's own costs them that file, not the rest.
    for (path, trust) in files {
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                // **A package runs when you have said it may, and not before.** Your own files
                // run on sight; this is for what arrived under `site/pack/` by being fetched,
                // and can change under you between one run and the next. casper is the program
                // whose declarations name commands, so this is the one where it matters most.
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

/// What a person gets for asking, and whether they got it.
///
/// Written rather than printed for the reason [`say`] is: `println!` panics on a write that
/// fails, so `casper help` with its stdout on a full device or a closed pipe exited 101 through
/// a panic handler instead of reporting undelivered like every other verb here.
fn usage() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(
        concat!(
            "casper — the tooling interface\n\
         \n\
         \x20 casper tools        every tool it offers, with schemas\n\
         \x20 casper run          one call on stdin, one result on stdout\n\
         \x20 casper verbs        what its socket answers\n\
         \n\
         \x20 --json | --cbor   which encoding a reply comes back in\n\
         \n\
         Every verb prints the family's reply shape. `run` is deliberately not\n\
         reachable over the socket: see DESIGN.md.\n"
        )
        .as_bytes(),
    )
    .and_then(|()| out.flush())
    .inspect_err(|why| casper::noted!("usage: it could not be written: {why}"))
    .is_ok()
}

/// Hold a tool's rows, exchanging frames until it is finished.
///
/// Nothing is printed in the reply shape here: this is a stream of frames, not a call, and a
/// client that read it as one would take the first frame for the whole answer.
fn held(tool: &str) -> bool {
    let Ok(mut engine) = loaded() else {
        // A half-finished install used to exit here without a word, leaving the harness holding
        // rows for a tenant that was never going to draw.
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
        // The property every door in the family owes a caller: the encoding changes, the shape
        // does not. casper answered in JSON alone, so a caller that asked melchior and balthasar
        // for CBOR still needed a JSON parser for this one.
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
        // A refusal is a reply, so it takes the same route. Worth pinning separately: the JSON
        // fallback here is hand-written, and a fallback nobody exercises is a fallback that
        // stops compiling into something valid.
        let reply = Reply::refused("no such tool: nope".to_owned());
        let from_cbor: serde_json::Value =
            ciborium::from_reader(encoded(As::Cbor, &reply).as_slice()).expect("cbor");
        assert_eq!(from_cbor["ok"], serde_json::json!(false));
    }
    #[test]
    fn a_listing_is_rows_and_not_one_row_that_is_a_list() {
        // The shape melchior and balthasar were already sending, and the one casper was not:
        // `result` is the rows. A coordinator that deserialised each row found an array where a
        // declaration should have been, took nothing from it, and reported casper as declaring
        // no settings at all.
        let reply = listing(serde_json::json!([{ "name": "tools" }, { "name": "load" }]));
        assert_eq!(reply.n, 2, "n is the number of rows");
        assert_eq!(reply.result.len(), 2);
        assert!(reply.result[0].is_object(), "a row is not a list");
    }

    #[test]
    fn n_is_the_number_of_rows_for_every_verb_that_lists() {
        // The invariant `n == result.len()`, on the real answers rather than a fixture, because
        // it was the count being wrong that made the wrapping visible from outside.
        for reply in [listing(described()), listing(needs())] {
            assert_eq!(reply.n, reply.result.len(), "{reply:?}");
            assert!(reply.n > 1, "a listing has rows: {reply:?}");
            for row in &reply.result {
                assert!(row.is_object(), "{row}");
            }
        }
    }
}
