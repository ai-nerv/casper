//! What a config cannot reach. `Lua::full()` hands the VM the whole standard library, including
//! the `os.execute` and `io.popen` that would let a Lua tool spawn without the process
//! transport. Removed here rather than never installed, so what the next luna release adds is
//! reachable until this list is extended.

use luna::{Callback, CallbackReturn, Context, Function, IntoValue, Lua, Value};

/// What a precompiled chunk is told, wherever it is handed in.
pub const PRECOMPILED: &str = "a precompiled chunk is refused: only Lua source is loaded";

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

/// Globals removed entirely. `io` goes wholesale: a tool that needs a file has `Ops`.
const REMOVED_TABLES: &[&str] = &["io", "package", "dofile", "loadfile", "require"];

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
        if let Value::Function(load) = ctx.get_global_value("load") {
            ctx.set_global("load", textual(ctx, load));
        }
    });
}

/// Whether `source` is a precompiled chunk: every Lua binary signature opens with an escape, and
/// no source text can.
#[must_use]
pub fn precompiled(source: &[u8]) -> bool {
    source.first() == Some(&0x1b)
}

/// `load`, refusing a precompiled chunk whatever mode it names, then handing over to the original.
fn textual<'gc>(ctx: Context<'gc>, load: Function<'gc>) -> Callback<'gc> {
    Callback::from_fn_with(&ctx, load, |load, ctx, _exec, stack| {
        if let Value::String(chunk) = stack.get(0) {
            if precompiled(chunk.as_bytes()) {
                return Err(PRECOMPILED.into_value(ctx).into());
            }
        }
        Ok(CallbackReturn::Call {
            function: *load,
            then: None,
        })
    })
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
