//! What a config cannot reach. `Lua::full()` hands the VM the whole standard library, including
//! the `os.execute` and `io.popen` that would let a Lua tool spawn without the process
//! transport. Removed here rather than never installed, so what the next luna release adds is
//! reachable until this list is extended.

use luna::{Lua, Value};

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
    });
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
