//! What a config cannot reach. `Lua::full()` hands the VM the whole standard library, including
//! the `os.execute` and `io.popen` that would let a Lua tool spawn without the process
//! transport. Removed here rather than never installed, so what the next luna release adds is
//! reachable until this list is extended.

use luna::{Callback, CallbackReturn, Context, IntoValue, Lua, MetaMethod, Table, Value};

/// What a precompiled chunk is told, wherever it is handed in.
pub const PRECOMPILED: &str = "a precompiled chunk is refused: only Lua source is loaded";

/// What a metatable carrying `__gc` is told: casper never runs finalizers.
const FINALIZED: &str = "a metatable with __gc is refused: finalizers never run here";

/// Globals a config must not have: the ones that spawn, that write outside the `Ops` seam where
/// path checking lives, or that end the process.
const REMOVED: &[(&str, &str)] = &[
    ("os", "execute"),
    ("os", "exit"),
    ("os", "remove"),
    ("os", "rename"),
    ("os", "tmpname"),
    ("os", "setlocale"),
];

/// Globals removed entirely. `io` goes wholesale: a tool that needs a file has `Ops`. `debug`
/// goes too: it reaches past the guards below and into the frames the budget meters.
const REMOVED_TABLES: &[&str] = &["io", "package", "dofile", "loadfile", "require", "debug"];

/// Globals replaced rather than taken away, because a declaration writing a diagnostic is
/// reasonable and where luna sends it is not: `print` writes to stdout, which carries the reply
/// and a surface's frames, and `warn` goes through `eprintln!`, which panics on a failed write.
/// Both go to [`crate::noted`] instead, where the rest of casper's diagnostics go.
const QUIETENED: &[&str] = &["print", "warn"];

/// Take away what a config must not be able to do.
pub fn apply(lua: &mut Lua) {
    lua.enter(|ctx| {
        for (table, field) in REMOVED {
            if let Value::Table(t) = ctx.get_global_value(table) {
                t.set(ctx, *field, Value::Nil).ok();
            }
        }
        for name in REMOVED_TABLES {
            ctx.set_global(name, Value::Nil);
        }
        for name in QUIETENED {
            ctx.set_global(name, quiet(ctx, name));
        }
        guard(ctx, ctx.globals(), "load", binary);
        guard(ctx, ctx.globals(), "setmetatable", finalized);
        hide_userdata_metatables(ctx, ctx.globals());
    });
}

/// `getmetatable`, answering `nil` for a userdata: luna attaches one shared metatable to every
/// error it raises as a userdata.
fn hide_userdata_metatables<'gc>(ctx: Context<'gc>, globals: Table<'gc>) {
    let Value::Function(original) = globals.get_value(ctx, "getmetatable") else {
        return;
    };
    let hidden = Callback::from_fn_with(&ctx, original, |original, ctx, _exec, mut stack| {
        if matches!(stack.get(0), Value::UserData(_)) {
            stack.replace(ctx, Value::Nil);
            return Ok(CallbackReturn::Return);
        }
        Ok(CallbackReturn::Call {
            function: *original,
            then: None,
        })
    });
    globals.set(ctx, "getmetatable", hidden).ok();
}

/// Whether `source` is a precompiled chunk: every Lua binary signature opens with an escape, and
/// no source text can.
#[must_use]
pub fn precompiled(source: &[u8]) -> bool {
    source.first() == Some(&0x1b)
}

/// A check on a call's first two arguments, naming why it refuses them.
type Refusal = for<'gc> fn(Context<'gc>, Value<'gc>, Value<'gc>) -> Option<&'static str>;

/// Put `refuses` in front of the function at `table[name]`, which then runs as it did.
fn guard<'gc>(ctx: Context<'gc>, table: Table<'gc>, name: &'static str, refuses: Refusal) {
    let Value::Function(original) = table.get_value(ctx, name) else {
        return;
    };
    let guarded = Callback::from_fn_with(&ctx, original, move |original, ctx, _exec, stack| {
        if let Some(why) = refuses(ctx, stack.get(0), stack.get(1)) {
            return Err(why.into_value(ctx).into());
        }
        Ok(CallbackReturn::Call {
            function: *original,
            then: None,
        })
    });
    table.set(ctx, name, guarded).ok();
}

/// `load` given a precompiled chunk, whatever mode it names.
fn binary<'gc>(_ctx: Context<'gc>, chunk: Value<'gc>, _: Value<'gc>) -> Option<&'static str> {
    matches!(chunk, Value::String(chunk) if precompiled(chunk.as_bytes())).then_some(PRECOMPILED)
}

/// A metatable carrying `__gc`, which would queue its object for a finalizer that never runs.
fn finalized<'gc>(ctx: Context<'gc>, _: Value<'gc>, metatable: Value<'gc>) -> Option<&'static str> {
    matches!(metatable, Value::Table(metatable) if !metatable.get_value(ctx, MetaMethod::Gc).is_nil())
        .then_some(FINALIZED)
}

/// A `print` that goes to the debug log, and nowhere at all when nobody turned one on.
fn quiet<'gc>(ctx: Context<'gc>, named: &'static str) -> Callback<'gc> {
    Callback::from_fn(&ctx, move |_ctx, _exec, mut stack| {
        if std::env::var_os(crate::noted::VARIABLE).is_some() {
            let mut said = String::new();
            for at in 0..stack.len() {
                if at > 0 {
                    said.push('\t');
                }
                said.push_str(&text(&stack.get(at)));
            }
            crate::noted::note(format_args!("{named}: {said}"));
        }
        stack.clear();
        Ok(CallbackReturn::Return)
    })
}

/// One value as text. No `__tostring`: a metamethod here would re-enter the VM from a diagnostic.
fn text(value: &Value<'_>) -> String {
    match value {
        Value::String(said) => said.display_lossy().to_string(),
        other => other.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::lua::engine::Engine;
    use std::collections::BTreeSet;

    /// What one expression evaluates to inside a fresh engine.
    fn probe(expression: &str) -> String {
        let mut engine = Engine::new();
        engine
            .run(
                &format!("casper.answer = tostring({expression})"),
                "probe.lua",
            )
            .expect("run");
        engine.harvest();
        engine
            .setting("answer")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "<absent>".to_owned())
    }

    #[test]
    fn a_config_cannot_spawn_a_process() {
        assert_eq!(probe("os.execute"), "nil");
        assert_eq!(probe("io"), "nil");
    }

    #[test]
    fn a_config_cannot_write_outside_the_ops_seam() {
        for expression in ["os.remove", "os.rename", "os.tmpname"] {
            assert_eq!(probe(expression), "nil", "{expression} is still reachable");
        }
    }

    #[test]
    fn a_config_cannot_end_the_daemon() {
        assert_eq!(probe("os.exit"), "nil");
    }

    #[test]
    fn a_config_cannot_load_arbitrary_files() {
        for expression in ["dofile", "loadfile", "require", "package"] {
            assert_eq!(probe(expression), "nil", "{expression} is still reachable");
        }
    }

    #[test]
    fn load_refuses_a_precompiled_chunk_whatever_mode_it_names() {
        let dumped = "string.dump(load('return 1'))";
        for chunk in [dumped, "'\\27Lua\\84\\0'"] {
            for mode in ["nil", "'b'", "'bt'"] {
                let said = probe(&format!("select(2, pcall(load, {chunk}, 'chunk', {mode}))"));
                assert!(said.contains("precompiled"), "{chunk} as {mode}: {said}");
            }
        }
        assert_eq!(probe("load('return 41 + 1')()"), "42");
    }

    #[test]
    fn a_precompiled_declaration_is_refused_before_it_is_compiled() {
        for source in ["\u{1b}Lua\u{54}\0", "\u{1b}Luna\0"] {
            let why = Engine::new().run(source, "tools.lua").expect_err("refused");
            assert!(why.to_string().contains("tools.lua"), "{why}");
            assert!(why.to_string().contains("precompiled"), "{why}");
        }
    }

    #[test]
    fn a_metatable_carrying_gc_is_refused_and_any_other_is_taken() {
        let said = probe("select(2, pcall(setmetatable, {}, { __gc = function() end }))");
        assert!(said.contains("__gc"), "{said}");
        let kept = "local t = {}; return setmetatable(t, { __index = { x = 7 } }) == t and t.x";
        assert_eq!(probe(&format!("(function() {kept} end)()")), "7");
        let locked = "setmetatable(setmetatable({}, { __metatable = 1 }), {})";
        assert!(probe(&format!("select(2, pcall(function() {locked} end))")).contains("protected"));
    }

    /// Every name a fresh declaration VM holds, two tables deep: globals, their fields, and those
    /// fields' fields. A luna bump or a new `casper.*` that adds one fails here until it is listed.
    const ALLOWED: &str = "
        _G _VERSION assert collectgarbage error getmetatable ipairs load next pairs pcall print
        rawequal rawget rawlen rawset select setmetatable tonumber tostring type warn xpcall
        __casper_file_renderers __casper_screens __casper_surfaces __casper_tools __stream __stream.connect
        casper casper.ask casper.clients casper.clients.hexe casper.clients.oslo casper.dirs
        casper.dirs.list casper.dirs.tree casper.exec casper.file_tool casper.fs casper.fs.ls casper.image
        casper.json casper.json.decode casper.json.encode casper.mcp casper.paint
        casper.paint.ansi casper.paint.code casper.paint.diff casper.paint.plain casper.seek
        casper.replace_source casper.source casper.stream casper.stream.connect
        casper.surface casper.tapped casper.tool casper.web casper.wonder
        coroutine coroutine.close coroutine.continue coroutine.create coroutine.isyieldable
        coroutine.resume coroutine.running coroutine.status coroutine.wrap coroutine.yield
        coroutine.yieldto
        math math.abs math.acos math.asin math.atan math.ceil math.cos math.deg math.exp
        math.floor math.fmod math.huge math.log math.max math.maxinteger math.min math.mininteger
        math.modf math.pi math.rad math.random math.randomseed math.sin math.sqrt math.tan
        math.tointeger math.type math.ult
        os os.clock os.date os.difftime os.getenv os.time
        string string.byte string.char string.dump string.find string.format string.gmatch
        string.gsub string.len string.lower string.match string.pack string.packsize string.rep
        string.reverse string.sub string.unpack string.upper
        table table.clear table.concat table.freeze table.insert table.interceptall
        table.isfrozen table.move table.pack table.remove table.sort table.unpack
        utf8 utf8.char utf8.charpattern utf8.codepoint utf8.codes utf8.len utf8.offset
    ";

    #[test]
    fn the_names_a_declaration_can_reach_are_exactly_the_allowlist() {
        let walk = "local names = {}
            local function walk(held, prefix, depth)
              for key, value in pairs(held) do
                local name = prefix .. tostring(key)
                names[#names + 1] = name
                if type(value) == 'table' and value ~= _G and depth < 2 then
                  walk(value, name .. '.', depth + 1)
                end
              end
            end
            walk(_G, '', 0)
            table.sort(names)
            casper.answer = table.concat(names, ' ')";
        let mut engine = Engine::new();
        engine.run(walk, "walk.lua").expect("run");
        engine.harvest();
        let answer = engine.setting("answer").expect("walked");
        let found: BTreeSet<&str> = answer.as_str().unwrap_or_default().split(' ').collect();
        let allowed: BTreeSet<&str> = ALLOWED.split_whitespace().collect();
        let added: Vec<_> = found.difference(&allowed).collect();
        let gone: Vec<_> = allowed.difference(&found).collect();
        assert!(added.is_empty(), "reachable and not allowed: {added:?}");
        assert!(gone.is_empty(), "allowed and no longer there: {gone:?}");
    }

    #[test]
    fn what_a_config_legitimately_needs_still_works() {
        assert_ne!(probe("os.getenv"), "nil", "reading the environment is fine");
        assert_ne!(probe("os.time"), "nil");
        assert_ne!(
            probe("load"),
            "nil",
            "the family's clients are loaded chunks"
        );
        assert_ne!(probe("string.format"), "nil");
        assert_ne!(probe("table.concat"), "nil");
        assert_ne!(probe("casper.json.encode"), "nil");
        assert_ne!(probe("casper.exec"), "nil");
    }
}
