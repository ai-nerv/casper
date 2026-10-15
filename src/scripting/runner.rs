//! Drives the monty interpreter through its states.
//!
//! A tool call a script makes is parked, and every call parked by the time the script waits is
//! handed to the host as one batch, then fed back by id. `open()` and `Path.read_text` are served
//! by the host's reader; anything that would write, and every other OS call, is refused.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use monty::{MontyRun, RunProgress};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyFileHandle, MontyObject,
    NameLookupResult, OsFunctionCall, PrintWriter, PrintWriterCallback, ResourceLimits,
    ResourceTracker,
};
use serde_json::Value;

use super::alloc::SandboxScope;
use super::convert::{json_to_monty, monty_to_json};

const MAX_RECURSION: usize = 100;
pub(super) const SCRIPT_NAME: &str = "script.py";
const OS_CALLS_DENIED: &str = "OS calls are not permitted";
const BINARY_FILES: &str = "binary files are not supported, open in text mode";
const READ_ONLY: &str = "a script cannot write files: call the write tool as a tool of its own";

#[derive(Debug, thiserror::Error)]
pub enum Failed {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("sandboxed: {0}")]
    Sandboxed(String),
}

/// One awaited call, as the script made it.
#[derive(Debug)]
pub struct PendingCall {
    pub call_id: u32,
    pub name: String,
    pub args: Vec<Value>,
    pub kwargs: Vec<(String, Value)>,
}

pub type Resolved = Vec<(u32, Result<Value, String>)>;

/// What a script may name, what it is told it may not, and who answers for both.
pub struct Bridge<'a> {
    pub callable: &'a HashSet<String>,
    /// A tool the script may not call, with the reason it is given when it tries.
    pub refused: &'a HashMap<String, String>,
    pub resolve: &'a dyn Fn(Vec<PendingCall>) -> Result<Resolved, Failed>,
    pub read: Option<&'a dyn Fn(String) -> Result<String, String>>,
}

/// What a script printed: the first `most` bytes, and a count of the rest.
#[derive(Debug, Default)]
pub struct Printed {
    pub text: String,
    pub dropped: usize,
    most: usize,
}

impl Printed {
    pub fn holding(most: usize) -> Self {
        Self {
            most,
            ..Self::default()
        }
    }

    fn take(&mut self, piece: &str) {
        let room = self.most.saturating_sub(self.text.len());
        if self.dropped == 0 && piece.len() <= room {
            self.text.push_str(piece);
            return;
        }
        let mut cut = if self.dropped == 0 { room } else { 0 };
        while !piece.is_char_boundary(cut) {
            cut -= 1;
        }
        self.text.push_str(piece.get(..cut).unwrap_or_default());
        self.dropped += piece.len() - cut;
    }
}

impl PrintWriterCallback for Printed {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.take(&output);
        Ok(())
    }

    fn stdout_push(&mut self, ch: char) -> Result<(), MontyException> {
        self.take(ch.encode_utf8(&mut [0; 4]));
        Ok(())
    }
}

/// Run `code` after `preamble`, as one script; tracebacks come back so line 1 is `code`'s.
pub fn run(
    code: &str,
    preamble: &str,
    bridge: &Bridge<'_>,
    limits: ResourceLimits,
    printed: &mut Printed,
) -> Result<Option<Value>, Failed> {
    let preamble = preamble.trim_end_matches('\n');
    let script = if preamble.is_empty() {
        code.to_owned()
    } else {
        format!("{preamble}\n{code}")
    };
    let offset = preamble.lines().count();
    let _sandbox = limits.max_memory.is_some().then(SandboxScope::enter);
    drive(script, bridge, limits, &mut PrintWriter::Callback(printed)).map_err(|e| match e {
        Failed::Parse(msg) => Failed::Parse(rebase_traceback(&msg, offset)),
        Failed::Runtime(msg) => Failed::Runtime(rebase_traceback(&msg, offset)),
        other @ Failed::Sandboxed(_) => other,
    })
}

fn runtime(e: impl std::fmt::Display) -> Failed {
    Failed::Runtime(e.to_string())
}

fn drive(
    script: String,
    bridge: &Bridge<'_>,
    limits: ResourceLimits,
    writer: &mut PrintWriter<'_>,
) -> Result<Option<Value>, Failed> {
    let runner = MontyRun::new(script, SCRIPT_NAME, vec![], CompileOptions::default())
        .map_err(|e| Failed::Parse(e.to_string()))?;
    let mut progress = runner
        .start(vec![], ResourceTracker::new(limits), writer.reborrow())
        .map_err(runtime)?;
    let mut parked: HashMap<u32, PendingCall> = HashMap::new();
    loop {
        progress = match progress {
            RunProgress::Complete(obj) => {
                return Ok((!matches!(obj, MontyObject::None)).then(|| monty_to_json(&obj)));
            }
            RunProgress::FunctionCall(call) => {
                let name = call.function_name.clone();
                if bridge.callable.contains(&name) {
                    let pending = PendingCall {
                        call_id: call.call_id,
                        args: call.args.iter().map(monty_to_json).collect(),
                        kwargs: call
                            .kwargs
                            .iter()
                            .map(|(k, v)| (k.to_string(), monty_to_json(v)))
                            .collect(),
                        name,
                    };
                    parked.insert(pending.call_id, pending);
                    call.resume_pending(writer.reborrow()).map_err(runtime)?
                } else if let Some(why) = bridge.refused.get(&name) {
                    let refused = MontyException::new(ExcType::PermissionError, Some(why.clone()));
                    call.resume(ExtFunctionResult::Error(refused), writer.reborrow())
                        .map_err(runtime)?
                } else {
                    call.resume(ExtFunctionResult::NotFound(name), writer.reborrow())
                        .map_err(runtime)?
                }
            }
            RunProgress::NameLookup(lookup) => {
                let name = &lookup.name;
                let known = bridge.callable.contains(name) || bridge.refused.contains_key(name);
                let result = if known {
                    NameLookupResult::Value(MontyObject::Function {
                        name: name.clone(),
                        docstring: None,
                    })
                } else {
                    NameLookupResult::Undefined
                };
                lookup.resume(result, writer.reborrow()).map_err(runtime)?
            }
            RunProgress::OsCall(call) => {
                let Some(read) = bridge.read else {
                    return Err(Failed::Sandboxed(OS_CALLS_DENIED.into()));
                };
                call.resume_with(writer.reborrow(), |os_call| serve(read, os_call))
                    .map_err(runtime)?
            }
            RunProgress::ResolveFutures(state) => {
                let batch: Vec<PendingCall> = state
                    .pending_call_ids()
                    .iter()
                    .filter_map(|id| parked.remove(id))
                    .collect();
                let results: Vec<(u32, ExtFunctionResult)> = (bridge.resolve)(batch)?
                    .into_iter()
                    .map(|(id, result)| match result {
                        Ok(value) => (id, ExtFunctionResult::Return(json_to_monty(value))),
                        Err(msg) => (
                            id,
                            ExtFunctionResult::Error(MontyException::new(
                                ExcType::RuntimeError,
                                Some(msg),
                            )),
                        ),
                    })
                    .collect();
                state.resume(results, writer.reborrow()).map_err(runtime)?
            }
        };
    }
}

/// One OS call: text reads through the host, writes refused, the rest refused as monty words it.
fn serve(
    read: &dyn Fn(String) -> Result<String, String>,
    call: OsFunctionCall,
) -> ExtFunctionResult {
    let writes = match &call {
        OsFunctionCall::Open(args) => args.mode.writable(),
        other => other.is_write(),
    };
    if writes {
        return ExtFunctionResult::Error(MontyException::new(
            ExcType::PermissionError,
            Some(READ_ONLY.into()),
        ));
    }
    let result = match call {
        OsFunctionCall::Open(args) if args.mode.is_binary() => Err(BINARY_FILES.to_owned()),
        OsFunctionCall::Open(args) => Ok(MontyObject::FileHandle(MontyFileHandle {
            path: args.path.into_string(),
            mode: args.mode,
            position: 0,
        })),
        OsFunctionCall::ReadText(path) => read(path.into_string()).map(MontyObject::String),
        other => return ExtFunctionResult::Error(other.on_no_handler()),
    };
    match result {
        Ok(value) => ExtFunctionResult::Return(value),
        Err(msg) => ExtFunctionResult::Error(MontyException::new(ExcType::OSError, Some(msg))),
    }
}

/// Frames from the preamble dropped, with their excerpts, and the rest renumbered from `code`.
pub(super) fn rebase_traceback(msg: &str, preamble_lines: usize) -> String {
    let prefix = format!("  File \"{SCRIPT_NAME}\", line ");
    let mut kept: Vec<Cow<'_, str>> = Vec::new();
    let mut in_preamble_frame = false;
    for line in msg.lines() {
        match line.strip_prefix(&prefix).and_then(split_line_number) {
            Some((number, rest)) => {
                in_preamble_frame = number <= preamble_lines;
                if !in_preamble_frame {
                    kept.push(format!("{prefix}{}{rest}", number - preamble_lines).into());
                }
            }
            None if in_preamble_frame && line.starts_with(' ') => {}
            None => {
                in_preamble_frame = false;
                kept.push(line.into());
            }
        }
    }
    kept.join("\n")
}

pub(super) fn split_line_number(rest: &str) -> Option<(usize, &str)> {
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, rest) = rest.split_at_checked(end)?;
    digits.parse().ok().map(|n| (n, rest))
}

pub fn limits(timeout: Duration, max_memory: usize) -> ResourceLimits {
    ResourceLimits::default()
        .max_duration(timeout)
        .max_memory(max_memory)
        .max_recursion_depth(MAX_RECURSION)
}
