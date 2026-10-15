//! `execute`: a Python script that calls casper's read-only tools as async functions.
//!
//! The script runs in monty, in-process, on a thread of its own. Every tool call it
//! makes goes through [`Host::run`], the path a direct call takes, so it is jailed, capped and
//! logged the same way. magi's gate sees `execute` and nothing it calls, so only a tool declared
//! `script = true` with `needs = "read"` is callable ([`callable`]): nothing a script reaches can
//! write, run a command or touch the network, whatever a declaration says.

mod alloc;
mod convert;
mod runner;
pub mod schema;
#[cfg(test)]
mod tests;

use crate::paint::{Role, Span};
use crate::tools::{Call, Card, Ran, Shown};
use runner::{Bridge, Failed, PendingCall, Printed, Resolved};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The tool's name, which is never callable from inside itself.
pub const TOOL: &str = "execute";
/// What the interpreter may hold.
pub const MEMORY: usize = 50 << 20;
/// Seconds of script time when a call names none, and the most it may name.
pub const SECONDS: u64 = 30;
pub const LONGEST: u64 = 600;
/// What a script's output is cut to.
pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;
/// Tool calls one script may make: monty's own suspension budget.
const MOST_CALLS: usize = monty_types::DEFAULT_MAX_SUSPENSIONS;
const AT_ONCE: usize = 8;
const OPEN_BYTES: usize = 8 << 20;
const STACK: usize = 32 << 20;
const TICK: Duration = Duration::from_millis(100);

const PREAMBLE: &str = "import re
import asyncio
import sys
import os
import json
async def gather(*calls):
    if len(calls) == 1 and isinstance(calls[0], list):
        calls = calls[0]
    results = []
    for c in calls:
        try:
            results.append(await c)
        except RuntimeError as e:
            results.append('[ERROR] ' + str(e))
    return results
";
const GATHER_HINT: &str =
    "\n\nHint: `gather(...)` keeps the other results, returning `[ERROR] ...` for the failed call.";
const TIME_LIMIT: &str = "time limit exceeded";
const NO_OUTPUT: &str = "(no output)";
const TOOLS_HEADER: &str =
    "\n\nAvailable tools (async Python functions, keyword arguments only):\n";
const NONE_CALLABLE: &str = "\n\nNo tool is callable from a script here.\n";
const IMAGE_NOTE: &str =
    "image pixels are not visible from a script; call read as a tool of its own";
const ABANDONED: &str = "the call was abandoned";

/// Whether a script may call `card`: declared for it, reading only, and named as Python names.
#[must_use]
pub fn callable(card: &Card) -> bool {
    card.script
        && card.needs.as_deref() == Some("read")
        && card.name != TOOL
        && identifier(&card.name)
}

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `execute`'s card, finished: its description gains the signature of each tool a script may
/// call among `cards`, the same set a script finds callable.
pub fn describe(cards: &mut [Card]) {
    let listed: Vec<String> = cards
        .iter()
        .filter(|card| callable(card))
        .map(|card| schema::signature(&card.name, &card.parameters))
        .collect();
    let Some(card) = cards.iter_mut().find(|card| card.name == TOOL) else {
        return;
    };
    if listed.is_empty() {
        card.description.push_str(NONE_CALLABLE);
        return;
    }
    card.description.push_str(TOOLS_HEADER);
    for line in listed {
        card.description.push_str(&line);
        card.description.push('\n');
    }
}

/// What a script runs against.
pub struct Host<'a> {
    /// Every tool on offer, callable from a script or not.
    pub cards: &'a [Card],
    /// The path one call takes, or why it was refused.
    pub run: &'a (dyn Fn(&Call) -> Result<Ran, String> + Sync),
    /// Whether whoever asked for this script has gone.
    pub abandoned: &'a dyn Fn() -> bool,
}

/// What the script's thread asks of the one that started it.
enum Asked {
    Calls(Vec<PendingCall>, mpsc::Sender<Result<Resolved, Failed>>),
    Read(String, mpsc::Sender<Result<String, String>>),
    Done(Printed, Result<Option<Value>, Failed>),
}

/// Run one script and say what it printed and returned.
pub fn run(code: &str, timeout: Option<i64>, host: &Host<'_>) -> Ran {
    let began = Instant::now();
    let seconds = timeout.map_or(SECONDS, |t| u64::try_from(t).unwrap_or(1).clamp(1, LONGEST));
    let (callable, refused) = names(host.cards);
    let (asks, asked) = mpsc::channel();
    let script = code.to_owned();
    let calls_allowed = callable.clone();
    let spawned = std::thread::Builder::new()
        .name("casper-execute".to_owned())
        .stack_size(STACK)
        .spawn(move || interpret(&script, seconds, &calls_allowed, &refused, &asks));
    if let Err(why) = spawned {
        return Ran::failed(format!("no thread for the script: {why}"));
    }
    let mut calls = 0;
    let (printed, result) = loop {
        match asked.recv_timeout(TICK) {
            Ok(Asked::Calls(batch, reply)) => {
                calls += batch.len();
                let answer = if calls > MOST_CALLS {
                    Err(Failed::Sandboxed(format!(
                        "more than {MOST_CALLS} tool calls in one script"
                    )))
                } else {
                    Ok(dispatch(batch, host))
                };
                let _ = reply.send(answer);
            }
            Ok(Asked::Read(path, reply)) => {
                let _ = reply.send(opened(&path, host, &callable));
            }
            Ok(Asked::Done(printed, result)) => break (printed, result),
            Err(mpsc::RecvTimeoutError::Timeout) if (host.abandoned)() => {
                crate::noted!("execute: abandoned after {calls} tool calls");
                return cut(code, "", "cancelled");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ran::failed("the script's thread ended without an answer");
            }
        }
    };
    crate::noted!(
        "execute: {} lines, {calls} tool calls, {} in {}ms",
        code.lines().count(),
        if result.is_ok() { "ok" } else { "failed" },
        began.elapsed().as_millis()
    );
    answered(code, seconds, printed, result)
}

/// The tools a script may call, and the reason it is given for each other tool it names.
fn names(cards: &[Card]) -> (HashSet<String>, HashMap<String, String>) {
    let (mut allowed, mut refused) = (HashSet::new(), HashMap::new());
    for card in cards.iter().filter(|card| identifier(&card.name)) {
        if callable(card) {
            allowed.insert(card.name.clone());
            continue;
        }
        let name = &card.name;
        let why = match card.needs.as_deref() {
            _ if name == TOOL => format!("{TOOL} cannot run a script from inside a script"),
            Some(verb @ ("write" | "run" | "reach")) => format!(
                "{name} is not callable from a script: it needs \"{verb}\", and a script only \
                 reads. Call {name} as a tool of its own"
            ),
            _ => format!("{name} is not callable from a script; call it as a tool of its own"),
        };
        refused.insert(name.clone(), why);
    }
    (allowed, refused)
}

/// The script's own thread: the interpreter, with each call and read handed back over `asks`.
fn interpret(
    code: &str,
    seconds: u64,
    callable: &HashSet<String>,
    refused: &HashMap<String, String>,
    asks: &mpsc::Sender<Asked>,
) {
    let abandoned = || Failed::Sandboxed(ABANDONED.to_owned());
    let resolve = |batch: Vec<PendingCall>| {
        let (reply, answer) = mpsc::channel();
        asks.send(Asked::Calls(batch, reply))
            .map_err(|_| abandoned())?;
        answer.recv().map_err(|_| abandoned())?
    };
    let read = |path: String| {
        let (reply, answer) = mpsc::channel();
        asks.send(Asked::Read(path, reply))
            .map_err(|_| ABANDONED.to_owned())?;
        answer.recv().map_err(|_| ABANDONED.to_owned())?
    };
    let bridge = Bridge {
        callable,
        refused,
        resolve: &resolve,
        read: Some(&read),
    };
    let mut printed = Printed::holding(MAX_BYTES + 1);
    let limits = runner::limits(Duration::from_secs(seconds), MEMORY);
    let result = runner::run(code, PREAMBLE, &bridge, limits, &mut printed);
    let _ = asks.send(Asked::Done(printed, result));
}

/// One batch of calls, at most [`AT_ONCE`] at a time, answered by id.
fn dispatch(batch: Vec<PendingCall>, host: &Host<'_>) -> Resolved {
    let (cards, run) = (host.cards, host.run);
    let mut out = Vec::with_capacity(batch.len());
    for group in batch.chunks(AT_ONCE) {
        std::thread::scope(|scope| {
            let running: Vec<_> = group
                .iter()
                .map(|call| (call, scope.spawn(move || inner(call, cards, run))))
                .collect();
            for (call, handle) in running {
                let result = handle
                    .join()
                    .unwrap_or_else(|_| Err(format!("{}: the call panicked", call.name)));
                out.push((call.call_id, result));
            }
        });
    }
    out
}

/// One call: its arguments held to the tool's schema, then run the way a direct call is.
fn inner(
    call: &PendingCall,
    cards: &[Card],
    run: &(dyn Fn(&Call) -> Result<Ran, String> + Sync),
) -> Result<Value, String> {
    let named = |why: String| format!("{}: {why}", call.name);
    let card = cards
        .iter()
        .find(|card| card.name == call.name && callable(card))
        .ok_or_else(|| named("not callable from a script".to_owned()))?;
    let given = arguments(&call.args, &call.kwargs).map_err(named)?;
    let args = schema::checked(&card.name, &card.parameters, given)?;
    let ran = run(&Call {
        tool: card.name.clone(),
        args,
        cwd: String::new(),
        answered: None,
    })
    .map_err(named)?;
    flattened(&call.name, ran)
}

/// The tool's input: keyword arguments, or one dict passed whole.
fn arguments(args: &[Value], kwargs: &[(String, Value)]) -> Result<Value, String> {
    if let Some(first) = args.first()
        && first.is_object()
    {
        return Ok(first.clone());
    }
    if !kwargs.is_empty() {
        return Ok(Value::Object(kwargs.iter().cloned().collect()));
    }
    if args.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    Err("pass arguments as keyword arguments (e.g. read(path='/file'))".to_owned())
}

/// What a script reads back from a call: the text, or the failure as an error.
fn flattened(name: &str, ran: Ran) -> Result<Value, String> {
    if ran.waiting() || matches!(ran.shown, Some(Shown::Surface(_))) {
        return Err(format!(
            "{name}: it needs an answer only the harness can give; call {name} as a tool of its own"
        ));
    }
    let mut said = ran.said;
    if !ran.images.is_empty() {
        said.push_str(&format!(" ({IMAGE_NOTE})"));
    }
    if ran.failed {
        Err(format!("{name}: {said}"))
    } else {
        Ok(Value::String(said))
    }
}

/// `open(path).read()`: allowed by the `read` tool, then read by the jailed reader it uses.
fn opened(path: &str, host: &Host<'_>, callable: &HashSet<String>) -> Result<String, String> {
    if !callable.contains("read") {
        return Err("open() reads through the read tool, which a script cannot call here".into());
    }
    let check = Call {
        tool: "read".to_owned(),
        args: serde_json::json!({ "path": path, "offset": 1, "limit": 1 }),
        cwd: String::new(),
        answered: None,
    };
    let ran = (host.run)(&check)?;
    if ran.failed {
        return Err(ran.said);
    }
    let most = OPEN_BYTES + 1;
    let argv = [
        "-c".to_owned(),
        most.to_string(),
        "--".to_owned(),
        path.to_owned(),
    ];
    let bytes = crate::running::binary("head", &argv, most)?;
    if bytes.len() > OPEN_BYTES {
        return Err(format!(
            "{path}: over {} MiB; read it in ranges with read(path=..., offset=..., limit=...)",
            OPEN_BYTES >> 20
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("{path}: not a text file"))
}

/// The result of a script that ran to an end.
fn answered(
    code: &str,
    seconds: u64,
    printed: Printed,
    result: Result<Option<Value>, Failed>,
) -> Ran {
    let out = printed.text.trim_end();
    match result {
        Ok(value) => {
            let mut said = out.to_owned();
            if let Some(value) = value {
                if !said.is_empty() {
                    said.push('\n');
                }
                said.push_str(&format!("return: {value}"));
            }
            if said.is_empty() && printed.dropped == 0 {
                said = NO_OUTPUT.to_owned();
            }
            finished(code, truncated(&said, printed.dropped), false)
        }
        Err(Failed::Runtime(why)) if why.contains(TIME_LIMIT) => {
            let shown = if out.is_empty() {
                String::new()
            } else {
                truncated(out, printed.dropped)
            };
            cut(code, &shown, &format!("timed out after {seconds}s"))
        }
        Err(why) => {
            let hint = if code.contains("asyncio.gather") {
                GATHER_HINT
            } else {
                ""
            };
            let error = truncated(&format!("{why}{hint}"), 0);
            let said = if out.is_empty() && printed.dropped == 0 {
                error
            } else {
                format!("{}\n{error}", truncated(out, printed.dropped))
            };
            finished(code, said, true)
        }
    }
}

/// A script stopped part way, with what it printed before the cut.
fn cut(code: &str, out: &str, reason: &str) -> Ran {
    let tail = if out.is_empty() {
        "no output before the cut"
    } else {
        "output above is partial"
    };
    let marker = format!("[{reason}; {tail}]");
    let said = if out.is_empty() {
        marker
    } else {
        format!("{out}\n{marker}")
    };
    finished(code, said, true)
}

fn finished(code: &str, said: String, failed: bool) -> Ran {
    let mut shown = crate::highlight::code(code, runner::SCRIPT_NAME);
    shown.push(vec![Span::new(Role::Dim, "──────")]);
    shown.extend(crate::paint::plain(&said));
    let lines = said.lines().count();
    let last = said
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let brief = format!(
        "{TOOL}: {} lines of Python, {lines} lines out; last: \"{}\"",
        code.lines().count(),
        crate::noted::short(last.trim())
    );
    Ran {
        said,
        failed,
        shown: Some(Shown::Painted { lines: shown }),
        brief: Some(brief),
        keep: failed,
        ..Ran::default()
    }
}

/// `text` cut to [`MAX_LINES`] and [`MAX_BYTES`], saying how many bytes it dropped, counting
/// `unseen` bytes that never reached it.
fn truncated(text: &str, unseen: usize) -> String {
    let mut out = String::new();
    for (at, line) in text.split('\n').enumerate() {
        if at >= MAX_LINES {
            break;
        }
        let joined = out.len() + usize::from(at > 0) + line.len();
        if joined > MAX_BYTES {
            if at == 0 {
                let mut end = MAX_BYTES;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(line.get(..end).unwrap_or_default());
            }
            break;
        }
        if at > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    let dropped = text.len() - out.len() + unseen;
    if dropped > 0 {
        out.push_str(&format!("\n\n[truncated {dropped} bytes]"));
    }
    out
}
