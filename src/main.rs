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

use casper::lua::engine::Engine;
use casper::tools::{Call, Ran};
use casper::wire::{Reply, VERBS};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or("help") {
        "verbs" => say(&Reply::of(described())),
        "tools" => say(&tools()),
        "run" => say(&ran()),
        // Not a call and not a reply: frames both ways for as long as the tool holds its rows.
        // See `casper::surface` for why this cannot be one exec per event.
        "surface" => held(args.get(1).map(String::as_str).unwrap_or_default()),
        "help" | "--help" | "-h" => usage(),
        other => say(&Reply::refused(format!("no such call: {other}"))),
    }
    std::process::ExitCode::SUCCESS
}

/// Print one reply, in the shape every client parses.
fn say(reply: &Reply) {
    match serde_json::to_string(reply) {
        Ok(line) => println!("{line}"),
        // Nothing else can be done from here, and silence is the one answer a client cannot
        // read: it would wait for a frame that never comes.
        Err(why) => println!(r#"{{"ok":false,"family":1,"n":0,"result":[],"error":"{why}"}}"#),
    }
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
