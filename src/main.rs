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

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let how = asked(&args);
    match args.first().map(String::as_str).unwrap_or("help") {
        "verbs" => say(how, &Reply::of(described())),
        "tools" => say(how, &tools()),
        "run" => say(how, &ran()),
        // Not a call and not a reply: frames both ways for as long as the tool holds its rows.
        // See `casper::surface` for why this cannot be one exec per event.
        "surface" => held(args.get(1).map(String::as_str).unwrap_or_default()),
        "help" | "--help" | "-h" => usage(),
        other => say(how, &Reply::refused(format!("no such call: {other}"))),
    }
    std::process::ExitCode::SUCCESS
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
fn say(how: As, reply: &Reply) {
    use std::io::Write;
    let _ = std::io::stdout().lock().write_all(&encoded(how, reply));
}

/// What the socket answers, as name and description.
fn described() -> serde_json::Value {
    serde_json::Value::Array(
        VERBS
            .iter()
            .map(|(name, about)| serde_json::json!({"verb": name, "about": about}))
            .collect(),
    )
}

/// Every tool, as a card.
fn tools() -> Reply {
    let engine = match loaded() {
        Ok(engine) => engine,
        Err(why) => return Reply::refused(why),
    };
    match serde_json::to_value(engine.tools()) {
        Ok(cards) => Reply::of(cards),
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
    // A tool nobody declared is a refusal rather than a failed result: the model asked for
    // something that does not exist, and telling it the call *failed* invites a retry.
    let Some(ran) = engine.call(&call.tool, &given(&call)) else {
        return Reply::refused(format!("no such tool: {}", call.tool));
    };
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
    // Shipped rather than looked for. A casper with no tools is not a casper, and a relative
    // path would load whichever checkout the working directory happened to be in — which is how
    // a sibling ends up running another project's declarations.
    engine
        .run(include_str!("../config/tools.lua"), "tools.lua")
        .map_err(|why| why.to_string())?;
    engine.harvest();
    Ok(engine)
}

/// What a person gets for asking.
fn usage() {
    println!(
        "casper — the tooling interface\n\
         \n\
         \x20 casper tools        every tool it offers, with schemas\n\
         \x20 casper run          one call on stdin, one result on stdout\n\
         \x20 casper verbs        what its socket answers\n\
         \n\
         \x20 --json | --cbor   which encoding a reply comes back in\n\
         \n\
         Every verb prints the family's reply shape. `run` is deliberately not\n\
         reachable over the socket: see DESIGN.md."
    );
}

/// Hold a tool's rows, exchanging frames until it is finished.
///
/// Nothing is printed in the reply shape here: this is a stream of frames, not a call, and a
/// client that read it as one would take the first frame for the whole answer.
fn held(tool: &str) {
    let Ok(mut engine) = loaded() else {
        return;
    };
    casper::surface::hold(tool, &mut engine);
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
}
