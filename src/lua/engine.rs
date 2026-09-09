//! The VM tools are declared in.
//!
//! A tool is a description and a function. The description — name, text, schema, verb — comes out
//! into Rust as a [`crate::tools::Card`]; the function stays in the VM and is called back into.
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
//! `os.execute` and `io.popen` stay removed: [`crate::lua::exec`] is the only way to a process.

use crate::tools::{Card, Ran};
use luna::{Callback, CallbackReturn, Closure, Executor, Lua, Table, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// Where declared tools' functions live, out of reach of a config that did not declare them.
const TOOLS: &str = "__casper_tools";

/// Where declared tools' `surface` openers live.
const SURFACES: &str = "__casper_surfaces";

/// Where declared tools' `screen` openers live: a program on a pty, filling a `surface`'s spot.
const SCREENS: &str = "__casper_screens";

/// The open surface's own function, for as long as it holds its rows.
const LIVE: &str = "__casper_live";

/// Where a frame is handed in, and what the surface drew handed back.
const EVENT: &str = "__casper_event";
/// Where a surface's answer to a frame comes back.
const DREW: &str = "__casper_drew";

/// The client libraries a declaration may `load`, as source. Copied, not ported.
const CLIENTS: &[(&str, &str)] = &[
    ("hexe", include_str!("../../config/clients/hexe.lua")),
    ("oslo", include_str!("../../config/clients/oslo.lua")),
];

const ARGS: &str = "__casper_args";
const RESULT: &str = "__casper_result";

/// Anything that can go wrong loading a declaration.
#[derive(Debug, thiserror::Error)]
pub enum LuaError {
    /// The chunk would not compile.
    #[error("{file}: {message}")]
    Syntax { file: String, message: String },
    /// The chunk compiled and raised while running.
    #[error("{file}: {message}")]
    Runtime { file: String, message: String },
}

/// What the declarations said.
#[derive(Debug, Default)]
pub struct Declared {
    /// Every tool, in the order it was declared.
    pub tools: Vec<Card>,
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

            // Functions live in a global of their own, out of reach of a later declaration.
            let held = Table::new(&ctx);
            ctx.set_global(TOOLS, held);
            let surfaces = Table::new(&ctx);
            ctx.set_global(SURFACES, surfaces);
            let screens = Table::new(&ctx);
            ctx.set_global(SCREENS, screens);

            {
                let declared = Rc::clone(&declared);
                let tool = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
                    let (name, spec): (Value, Value) = stack.consume(ctx)?;
                    let (Value::String(name), Value::Table(spec)) = (name, spec) else {
                        return Err(raise(ctx, "casper.tool(name, spec): a name and a table"));
                    };
                    let name = String::from_utf8_lossy(name.as_bytes()).into_owned();

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
                    // The opener for a tool that fills rows of its own; absent on ordinary tools.
                    if let Value::Table(held) = ctx.get_global_value(SURFACES) {
                        let opens = spec.get_value(ctx, "surface");
                        held.set(ctx, name.as_str(), opens).ok();
                    }
                    // The same for a tool running a program in its rows. Never both; `screen` wins.
                    if let Value::Table(held) = ctx.get_global_value(SCREENS) {
                        let opens = spec.get_value(ctx, "screen");
                        held.set(ctx, name.as_str(), opens).ok();
                    }
                    // A hidden tool is registered and never described, so no model can call it.
                    let hidden = matches!(spec.get_value(ctx, "hidden"), Value::Boolean(true));
                    let mut declared = declared.borrow_mut();
                    declared.tools.retain(|held| held.name != card.name);
                    if !hidden {
                        declared.tools.push(card);
                    }
                    stack.replace(ctx, ());
                    Ok(CallbackReturn::Return)
                });
                casper.set(ctx, "tool", tool).ok();
            }

            // The socket primitive, named twice for a client stub that does not know this host.
            let stream = crate::lua::stream::table(ctx);
            casper.set(ctx, "stream", stream).ok();
            ctx.set_global("__stream", stream);
            casper.set(ctx, "fs", crate::lua::fs::table(ctx)).ok();
            // A declaration cannot open a file, so a stub arrives as text and is `load`ed.
            let clients = Table::new(&ctx);
            for (name, source) in CLIENTS {
                clients
                    .set(
                        ctx,
                        *name,
                        luna::String::from_slice(&ctx, source.as_bytes()),
                    )
                    .ok();
            }
            casper.set(ctx, "clients", clients).ok();
            casper.set(ctx, "ask", crate::lua::ask::table(ctx)).ok();
            casper
                .set(ctx, "surface", crate::lua::surface::table(ctx))
                .ok();
            // Under the Kitty protocol a keystroke arrives twice. See [`crate::lua::keying`].
            casper
                .set(ctx, "tapped", crate::lua::keying::table(ctx))
                .ok();
            casper.set(ctx, "exec", crate::lua::exec::table(ctx)).ok();
            casper.set(ctx, "json", crate::lua::json::table(ctx)).ok();
            casper.set(ctx, "paint", crate::lua::paint::table(ctx)).ok();

            ctx.set_global("casper", casper);
        });
    }

    /// Put one more callable on the `casper` table, after the VM is built, for a capability that
    /// only exists inside a surface. Registered from [`crate::surface::hold`], which is what keeps
    /// this module from knowing what a surface is.
    pub fn lend(&mut self, name: &str, build: fn(luna::Context<'_>) -> Callback<'_>) {
        let name = name.to_owned();
        self.lua.enter(|ctx| {
            let Ok(Value::Table(casper)) = ctx.get_global("casper") else {
                return;
            };
            casper.set(ctx, name.as_str(), build(ctx)).ok();
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
    /// `None` when no tool of that name was declared, or its `run` is not a function. A tool that
    /// raises comes back as a failed [`Ran`]: the raise is what the model should read.
    pub fn call(&mut self, name: &str, args: &serde_json::Value) -> Option<Ran> {
        if !self.declared.borrow().tools.iter().any(|t| t.name == name) {
            return None;
        }
        self.lua.enter(|ctx| {
            let value = crate::lua::convert::lua_from_json(ctx, args);
            ctx.set_global(ARGS, value);
            ctx.set_global(RESULT, Value::Nil);
        });

        // Called through a chunk, so a raise is caught the way a syntax error is.
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
        // A tool may answer in the wire's shape, or with a bare string when text is all it has.
        let mut ran = match out {
            serde_json::Value::String(said) => Ran::said(said),
            other => serde_json::from_value(other).unwrap_or_else(|why| {
                Ran::failed(format!("{name} answered something unreadable: {why}"))
            }),
        };
        self.ticking(name, &mut ran);
        Some(ran)
    }

    /// Make sure a tool that runs a *program* in its rows will be woken to read it.
    ///
    /// A screen always ticks: with no tick nothing looks at what the program painted and the rows
    /// freeze. Filled in only when the declaration named no rate of its own.
    fn ticking(&mut self, name: &str, ran: &mut Ran) {
        /// Thirty frames a second: the rate the rows are read, not the rate anything redraws.
        const A_SCREEN: u16 = 33;

        let Some(crate::tools::Shown::Surface(surface)) = ran.shown.as_mut() else {
            return;
        };
        if surface.tick.is_some() {
            return;
        }
        let mut screened = false;
        self.lua.enter(|ctx| {
            if let Value::Table(held) = ctx.get_global_value(SCREENS) {
                screened = matches!(held.get_value(ctx, name), Value::Function(_));
            }
        });
        if screened {
            surface.tick = Some(A_SCREEN);
        }
    }

    /// Ask a tool what program belongs in its rows.
    ///
    /// Calls the tool's `screen(args, size)`, which returns data — a command and its arguments —
    /// rather than a closure the way `surface` does. `None` when the tool declared no `screen`.
    pub fn screen(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        size: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.lua.enter(|ctx| {
            let value = crate::lua::convert::lua_from_json(ctx, args);
            ctx.set_global(ARGS, value);
            let size = crate::lua::convert::lua_from_json(ctx, size);
            ctx.set_global(EVENT, size);
            ctx.set_global(DREW, Value::Nil);
        });
        let source = format!(
            "local open = {SCREENS} and {SCREENS}[{name:?}]\n\
             if type(open) == 'function' then {DREW} = open({ARGS}, {EVENT}) end"
        );
        self.run(&source, "screen.lua").ok()?;
        let mut out = None;
        self.lua.enter(|ctx| {
            out = crate::lua::convert::json_from_lua(ctx, ctx.get_global_value(DREW), 0);
        });
        out.filter(|value| !value.is_null())
    }

    /// Open a tool's surface and hold it.
    ///
    /// Calls the tool's `surface(args, size)`, which returns *a function*: the tenant keeps its
    /// state in that closure's upvalues. `size` is what the harness granted — rows, columns, and
    /// whether the keyboard reports holds. `false` when the tool declared no `surface`.
    pub fn open(&mut self, name: &str, args: &serde_json::Value, size: &serde_json::Value) -> bool {
        self.lua.enter(|ctx| {
            let value = crate::lua::convert::lua_from_json(ctx, args);
            ctx.set_global(ARGS, value);
            let size = crate::lua::convert::lua_from_json(ctx, size);
            ctx.set_global(EVENT, size);
            ctx.set_global(LIVE, Value::Nil);
        });
        let source = format!(
            "local open = {SURFACES} and {SURFACES}[{name:?}]\n\
             if type(open) == 'function' then {LIVE} = open({ARGS}, {EVENT}) end"
        );
        if self.run(&source, "surface.lua").is_err() {
            return false;
        }
        let mut live = false;
        self.lua.enter(|ctx| {
            live = matches!(
                ctx.get_global_value(LIVE),
                Value::Function(_) | Value::Table(_)
            );
        });
        live
    }

    /// Hand the open surface one frame, and take what it drew.
    ///
    /// `None` when nothing is open or the tenant raised; both end the reservation.
    pub fn frame(&mut self, event: &serde_json::Value) -> Option<serde_json::Value> {
        self.lua.enter(|ctx| {
            let value = crate::lua::convert::lua_from_json(ctx, event);
            ctx.set_global(EVENT, value);
            ctx.set_global(DREW, Value::Nil);
        });
        let source = format!("if {LIVE} then {DREW} = {LIVE}({EVENT}) end");
        if self.run(&source, "surface.lua").is_err() {
            return None;
        }
        let mut out = None;
        self.lua.enter(|ctx| {
            out = crate::lua::convert::json_from_lua(ctx, ctx.get_global_value(DREW), 0);
        });
        out.filter(|value| !value.is_null())
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
                // A registrar is a function and cannot be described; anything else is a setting.
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

    /// Every name a declaration assigned.
    #[must_use]
    pub fn settings(&self) -> Vec<String> {
        self.declared.borrow().settings.keys().cloned().collect()
    }
}

/// Read a declaration's data half.
fn card<'gc>(ctx: luna::Context<'gc>, name: &str, spec: Table<'gc>) -> Option<Card> {
    let text = |key: &str| match spec.get_value(ctx, key) {
        Value::String(s) => Some(String::from_utf8_lossy(s.as_bytes()).into_owned()),
        _ => None,
    };
    Some(Card {
        name: name.to_owned(),
        description: text("description").unwrap_or_default(),
        // A tool with no schema takes no arguments, which is a real thing to be.
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

    fn declaring(source: &str) -> Engine {
        let mut engine = Engine::new();
        engine.run(source, "test.lua").expect("the chunk runs");
        engine
    }

    #[test]
    fn a_declaration_becomes_a_card_without_entering_the_vm_again() {
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
        let mut engine = declaring(r#"casper.tool("pwd", { run = function() return "/tmp" end })"#);
        let ran = engine.call("pwd", &serde_json::Value::Null).expect("ran");
        assert_eq!(ran.said, "/tmp");
        assert!(ran.shown.is_none());
    }

    #[test]
    fn a_tool_that_raises_is_a_result_the_model_reads() {
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
        let mut engine = declaring("casper.pager = \"bat\"\ncasper.pager = \"less\"");
        engine.harvest();
        assert_eq!(engine.setting("pager"), Some(serde_json::json!("less")));
    }
}
