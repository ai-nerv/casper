//! The VM tools are declared in.
//!
//! A tool is a description and a function. The description — a name, what it does, its schema,
//! the verb it acts under — is data, and comes out into Rust as a [`crate::tools::Card`]. The
//! function stays in the VM, because a function cannot be described as data, and is called back
//! into when somebody runs the tool.
//!
//! ```lua
//! casper.tool("cat", {
//!   description = "Read a file, with syntax highlighting.",
//!   parameters  = { type = "object", properties = { path = { type = "string" } },
//!                   required = { "path" } },
//!   needs       = "read",
//!   run = function(args) return { said = casper.exec("bat", { args.path }).out } end,
//! })
//! ```
//!
//! # Why the sandbox stays, in the tool runner of all places
//!
//! `os.execute` and `io.popen` are removed here exactly as they are in the siblings, even though
//! running programs is casper's entire job. A declaration that could spawn directly would spawn
//! *outside* everything casper is for: no bound on the output, no cancellation, nothing recorded,
//! and no permission verb attached. [`crate::lua::exec`] is the way through, and it is a way that
//! can be given rules. A second way with none would make the first decoration.

use crate::tools::{Card, Ran};
use luna::{Callback, CallbackReturn, Closure, Executor, Lua, Table, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// Where declared tools' functions live, out of reach of a config that did not declare them.
const TOOLS: &str = "__casper_tools";

/// Where a call's arguments and its answer are handed across.
const ARGS: &str = "__casper_args";
/// Where a call's answer comes back.
const RESULT: &str = "__casper_result";

/// Anything that can go wrong loading a declaration.
#[derive(Debug, thiserror::Error)]
pub enum LuaError {
    /// The chunk would not compile. Fatal, and it names the file: a declaration that does not
    /// parse has not expressed an intention, so guessing at one is worse than stopping.
    #[error("{file}: {message}")]
    Syntax {
        /// The file that would not compile.
        file: String,
        /// What the parser said.
        message: String,
    },
    /// The chunk compiled and raised while running.
    #[error("{file}: {message}")]
    Runtime {
        /// The file that raised.
        file: String,
        /// What was raised.
        message: String,
    },
}

/// What the declarations said.
#[derive(Debug, Default)]
pub struct Declared {
    /// Every tool, in the order it was declared.
    pub tools: Vec<Card>,
    /// Settings assigned onto `casper`, harvested after the chunk ran.
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// A VM with casper's surface installed.
pub struct Engine {
    lua: Lua,
    declared: Rc<RefCell<Declared>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// A VM with the standard library trimmed and `casper` installed.
    #[must_use]
    pub fn new() -> Self {
        let mut engine = Self {
            lua: Lua::full(),
            declared: Rc::new(RefCell::new(Declared::default())),
        };
        crate::lua::sandbox::apply(&mut engine.lua);
        engine.install();
        engine
    }

    /// Put `casper` in front of a declaration.
    fn install(&mut self) {
        let declared = Rc::clone(&self.declared);
        self.lua.enter(|ctx| {
            let casper = Table::new(&ctx);

            // Declared tools' functions live in a global of their own rather than on `casper`,
            // so a later declaration cannot read one out and call it with arguments nobody
            // checked.
            let held = Table::new(&ctx);
            ctx.set_global(TOOLS, held);

            {
                let declared = Rc::clone(&declared);
                let tool = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                    let (name, spec): (Value, Value) = stack.consume(ctx)?;
                    let (Value::String(name), Value::Table(spec)) = (name, spec) else {
                        return Err(raise(ctx, "casper.tool(name, spec): a name and a table"));
                    };
                    let name = String::from_utf8_lossy(name.as_bytes()).into_owned();

                    // The description is data and comes out; the runner is a function and
                    // stays. Splitting them here is what lets Rust answer `tools()` without
                    // entering the VM at all.
                    let card = card(ctx, &name, spec);
                    let Some(card) = card else {
                        return Err(raise(
                            ctx,
                            &format!("casper.tool({name}): this table cannot be described"),
                        ));
                    };
                    if let Value::Table(held) = ctx.get_global_value(TOOLS) {
                        let run = spec.get_value(ctx, "run");
                        held.set(ctx, name.as_str(), run).ok();
                    }
                    let mut declared = declared.borrow_mut();
                    declared.tools.retain(|held| held.name != card.name);
                    declared.tools.push(card);
                    stack.replace(ctx, ());
                    Ok(CallbackReturn::Return)
                });
                casper.set(ctx, "tool", tool).ok();
            }

            // The one way a declaration reaches a process. See the module docs: a second way,
            // with no bound and no verb attached, would make this one decoration.
            casper.set(ctx, "exec", crate::lua::exec::table(ctx)).ok();
            // Every tool that reads a program's output reads JSON somewhere; lending one parser
            // beats each of them carrying its own.
            casper.set(ctx, "json", crate::lua::json::table(ctx)).ok();
            // The paint vocabulary, so a declaration can say what its output *means* without
            // knowing what colour anybody will draw it in.
            casper.set(ctx, "paint", crate::lua::paint::table(ctx)).ok();

            ctx.set_global("casper", casper);
        });
    }

    /// Run one chunk.
    ///
    /// # Errors
    /// When it will not compile, or raises.
    pub fn run(&mut self, source: &str, file: &str) -> Result<(), LuaError> {
        let executor = self
            .lua
            .try_enter(|ctx| {
                let closure = Closure::load(ctx, Some(file), source.as_bytes())?;
                Ok(ctx.stash(Executor::start(ctx, closure.into(), ())))
            })
            .map_err(|why| LuaError::Syntax {
                file: file.to_owned(),
                message: why.to_string(),
            })?;
        self.lua
            .execute::<()>(&executor)
            .map_err(|why| LuaError::Runtime {
                file: file.to_owned(),
                message: why.to_string(),
            })
    }

    /// What has been declared so far.
    #[must_use]
    pub fn tools(&self) -> Vec<Card> {
        self.declared.borrow().tools.clone()
    }

    /// Run one declared tool.
    ///
    /// `None` when no tool of that name was declared, or its `run` is not a function — both of
    /// which are the same answer to a caller: casper cannot do this, and saying which would be
    /// telling the model about a config it cannot fix.
    ///
    /// A tool that raises comes back as a failed [`Ran`] rather than as an error, because a
    /// raise is something the model should read: it is what went wrong, and it is often
    /// actionable.
    pub fn call(&mut self, name: &str, args: &serde_json::Value) -> Option<Ran> {
        if !self.declared.borrow().tools.iter().any(|t| t.name == name) {
            return None;
        }
        self.lua.enter(|ctx| {
            let value = crate::lua::convert::lua_from_json(ctx, args);
            ctx.set_global(ARGS, value);
            ctx.set_global(RESULT, Value::Nil);
        });

        // Called through a chunk rather than by reaching into the stack, so a tool that raises
        // is caught by the same path a syntax error is and comes back as a message.
        let source = format!(
            "local fn = {TOOLS} and {TOOLS}[{name:?}]\n\
             if type(fn) == 'function' then {RESULT} = fn({ARGS}) end"
        );
        if let Err(why) = self.run(&source, "tool.lua") {
            return Some(Ran::failed(why.to_string()));
        }

        let mut out = None;
        self.lua.enter(|ctx| {
            out = crate::lua::convert::json_from_lua(ctx, ctx.get_global_value(RESULT), 0);
        });
        let out = out.filter(|value| !value.is_null())?;
        // A tool may answer in the wire's own shape, or with a bare string when it has nothing
        // to say beyond the text. The second is not a shorthand worth refusing: most tools have
        // exactly one thing to report.
        Some(match out {
            serde_json::Value::String(said) => Ran::said(said),
            other => serde_json::from_value(other).unwrap_or_else(|why| {
                Ran::failed(format!("{name} answered something unreadable: {why}"))
            }),
        })
    }

    /// Settings assigned onto `casper`, read back after the chunk ran.
    pub fn harvest(&mut self) {
        let declared = Rc::clone(&self.declared);
        self.lua.enter(|ctx| {
            let Value::Table(casper) = ctx.get_global_value("casper") else {
                return;
            };
            let mut declared = declared.borrow_mut();
            for (key, value) in casper.iter(ctx) {
                let Value::String(name) = key else { continue };
                let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
                // A registrar is a function and cannot be described; skipping it is what makes
                // "every other field is a setting" work without a list to keep in step.
                if let Some(json) = crate::lua::convert::json_from_lua(ctx, value, 0) {
                    declared.settings.insert(name, json);
                }
            }
        });
    }

    /// One setting, if a declaration assigned it.
    #[must_use]
    pub fn setting(&self, name: &str) -> Option<serde_json::Value> {
        self.declared.borrow().settings.get(name).cloned()
    }
}

/// Read a declaration's data half.
///
/// `None` when it is not describable — a table holding a function where a schema belongs, say.
fn card<'gc>(ctx: luna::Context<'gc>, name: &str, spec: Table<'gc>) -> Option<Card> {
    let text = |key: &str| match spec.get_value(ctx, key) {
        Value::String(s) => Some(String::from_utf8_lossy(s.as_bytes()).into_owned()),
        _ => None,
    };
    Some(Card {
        name: name.to_owned(),
        description: text("description").unwrap_or_default(),
        // A tool with no schema takes no arguments, which is a real thing to be and not an
        // error: `casper.tool("pwd", { run = ... })` should work.
        parameters: crate::lua::convert::json_from_lua(ctx, spec.get_value(ctx, "parameters"), 0)
            .filter(|value| !value.is_null())
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        needs: text("needs"),
    })
}

/// Raise a message into Lua.
fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An engine with `source` already run.
    fn declaring(source: &str) -> Engine {
        let mut engine = Engine::new();
        engine.run(source, "test.lua").expect("the chunk runs");
        engine
    }

    #[test]
    fn a_declaration_becomes_a_card_without_entering_the_vm_again() {
        // The split this file exists for: the description is data and comes out, so `tools()` is
        // answerable from Rust alone -- which is what lets the socket answer it without ever
        // running anything.
        let engine = declaring(
            r#"casper.tool("cat", {
                 description = "Read a file.",
                 parameters = { type = "object", properties = { path = { type = "string" } } },
                 needs = "read",
                 run = function(args) return "never called" end,
               })"#,
        );
        let tools = engine.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "cat");
        assert_eq!(tools[0].description, "Read a file.");
        assert_eq!(tools[0].needs.as_deref(), Some("read"));
        assert_eq!(tools[0].parameters["properties"]["path"]["type"], "string");
    }

    #[test]
    fn a_tool_runs_and_answers_in_the_wires_own_shape() {
        let mut engine = declaring(
            r#"casper.tool("echo", {
                 run = function(args) return { said = "you said " .. args.what } end,
               })"#,
        );
        let ran = engine
            .call("echo", &serde_json::json!({"what": "hello"}))
            .expect("it ran");
        assert_eq!(ran.said, "you said hello");
        assert!(!ran.failed);
    }

    #[test]
    fn a_bare_string_is_a_result_with_nothing_else_to_say() {
        // Most tools have exactly one thing to report, and making every one of them write
        // `{ said = ... }` would be ceremony for the common case.
        let mut engine = declaring(r#"casper.tool("pwd", { run = function() return "/tmp" end })"#);
        let ran = engine.call("pwd", &serde_json::Value::Null).expect("ran");
        assert_eq!(ran.said, "/tmp");
        assert!(ran.shown.is_none());
    }

    #[test]
    fn a_tool_that_raises_is_a_result_the_model_reads() {
        // Not an error the caller has to invent a message for. What was raised is usually what
        // the model needs in order to try something else.
        let mut engine =
            declaring(r#"casper.tool("nope", { run = function() error("no such thing") end })"#);
        let ran = engine.call("nope", &serde_json::Value::Null).expect("ran");
        assert!(ran.failed, "{ran:?}");
        assert!(ran.said.contains("no such thing"), "{}", ran.said);
    }

    #[test]
    fn a_tool_nobody_declared_is_not_a_tool() {
        let mut engine = Engine::new();
        assert!(engine.call("ghost", &serde_json::Value::Null).is_none());
    }

    #[test]
    fn declaring_the_same_name_twice_replaces_rather_than_appends() {
        // Registration is keyed. Re-running a declaration -- which reloading a config does --
        // must not leave two tools of one name for the model to choose between.
        let engine = declaring(
            r#"casper.tool("ls", { description = "one" })
               casper.tool("ls", { description = "two" })"#,
        );
        assert_eq!(engine.tools().len(), 1);
        assert_eq!(engine.tools()[0].description, "two");
    }

    #[test]
    fn a_tool_with_no_schema_takes_no_arguments_rather_than_failing() {
        let engine = declaring(r#"casper.tool("pwd", { run = function() return "/" end })"#);
        assert_eq!(engine.tools()[0].parameters["type"], "object");
    }

    #[test]
    fn a_declaration_that_will_not_parse_says_which_file() {
        let mut engine = Engine::new();
        let why = engine
            .run("this is not lua at all !!", "tools.lua")
            .expect_err("fails");
        assert!(why.to_string().contains("tools.lua"), "{why}");
    }

    #[test]
    fn a_setting_is_assigned_and_read_back_after_the_chunk_ran() {
        // The family's config style: settings are assigned, not declared in a table, and a
        // chunk may assign, read and re-assign its own before it finishes.
        let mut engine = declaring("casper.pager = \"bat\"\ncasper.pager = \"less\"");
        engine.harvest();
        assert_eq!(engine.setting("pager"), Some(serde_json::json!("less")));
    }
}
