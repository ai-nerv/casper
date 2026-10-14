//! Turning what the config declared into owned Rust values: luna's `Value<'gc>` cannot leave
//! `lua.enter`, so everything above this crate sees `serde_json::Value`.

use luna::{Table, Value};
use serde_json::{Map, Number};
use std::collections::HashSet;

/// The deepest one conversion may nest tables.
pub const DEEPEST: usize = 32;

/// The most values one conversion may write, keys included.
pub const NODES: usize = 1_000_000;

/// The most string bytes one conversion may write, keys included.
pub const BYTES: usize = 64 << 20;

/// A Lua value as JSON.
///
/// Lua does not distinguish a list from a map: a table whose keys are exactly `1..n` becomes an
/// array, anything else an object.
///
/// # Errors
/// When the value holds a function or a key that is not a name, holds itself, nests deeper than
/// [`DEEPEST`], or comes to more than [`NODES`] values or [`BYTES`] of text.
pub fn json_from_lua<'gc>(
    ctx: luna::Context<'gc>,
    value: Value<'gc>,
) -> Result<serde_json::Value, String> {
    Ok(Walk::new(ctx, false)
        .value(value)?
        .unwrap_or(serde_json::Value::Null))
}

/// One conversion's running totals, and the tables it is inside.
struct Walk<'gc> {
    ctx: luna::Context<'gc>,
    nodes: usize,
    bytes: usize,
    open: HashSet<Table<'gc>>,
    /// Drop what cannot be described, rather than refuse the whole value.
    dropping: bool,
}

impl<'gc> Walk<'gc> {
    fn new(ctx: luna::Context<'gc>, dropping: bool) -> Self {
        Self {
            ctx,
            nodes: 0,
            bytes: 0,
            open: HashSet::new(),
            dropping,
        }
    }

    /// One value, or `None` for one that is dropped.
    fn value(&mut self, value: Value<'gc>) -> Result<Option<serde_json::Value>, String> {
        self.spend()?;
        Ok(Some(match value {
            Value::Nil => serde_json::Value::Null,
            Value::Boolean(b) => serde_json::Value::Bool(b),
            Value::Integer(i) => serde_json::Value::Number(i.into()),
            Value::Number(f) => Number::from_f64(f).map_or(serde_json::Value::Null, Into::into),
            Value::String(s) => serde_json::Value::String(self.text(s)?),
            Value::Table(t) => return self.table(t),
            _ if self.dropping => return Ok(None),
            other => return Err(format!("a {} cannot be described", other.type_name())),
        }))
    }

    fn spend(&mut self) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > NODES {
            return Err(format!("more than {NODES} values"));
        }
        Ok(())
    }

    fn text(&mut self, text: luna::String<'gc>) -> Result<String, String> {
        self.bytes += text.as_bytes().len();
        if self.bytes > BYTES {
            return Err(format!("more than {} MiB of text", BYTES >> 20));
        }
        Ok(String::from_utf8_lossy(text.as_bytes()).into_owned())
    }

    fn table(&mut self, table: Table<'gc>) -> Result<Option<serde_json::Value>, String> {
        if self.open.len() >= DEEPEST {
            return Err(format!("nested deeper than {DEEPEST} tables"));
        }
        if !self.open.insert(table) {
            return Err("a table holds itself".to_owned());
        }
        // One past what the budget has left, so an over-wide table is refused unread.
        let room = NODES.saturating_sub(self.nodes) + 1;
        let entries: Vec<(Value<'gc>, Value<'gc>)> = table.iter(self.ctx).take(room).collect();
        let out = self.entries(entries);
        self.open.remove(&table);
        out.map(Some)
    }

    fn entries(
        &mut self,
        entries: Vec<(Value<'gc>, Value<'gc>)>,
    ) -> Result<serde_json::Value, String> {
        let is_list = !entries.is_empty()
            && entries.iter().enumerate().all(
                |(index, (key, _))| matches!(key, Value::Integer(i) if *i == index as i64 + 1),
            );

        if is_list {
            let mut out = Vec::with_capacity(entries.len());
            for (_, value) in entries {
                out.extend(self.value(value)?);
            }
            return Ok(serde_json::Value::Array(out));
        }

        let mut out = Map::new();
        for (key, value) in entries {
            self.spend()?;
            let name = match key {
                Value::String(s) => self.text(s)?,
                Value::Integer(i) => i.to_string(),
                _ if self.dropping => continue,
                other => {
                    return Err(format!(
                        "a key of type {} cannot be described",
                        other.type_name()
                    ));
                }
            };
            if let Some(json) = self.value(value)? {
                out.insert(name, json);
            }
        }
        Ok(serde_json::Value::Object(out))
    }
}

/// Read a declaration out of a Lua table.
pub trait FromLua: Sized {
    /// Build from the JSON a config table converted to.
    ///
    /// # Errors
    /// When the table does not match the type's shape.
    fn from_json(value: serde_json::Value) -> Result<Self, String>;
}

impl<T: serde::de::DeserializeOwned> FromLua for T {
    fn from_json(value: serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }
}

/// A JSON value as Lua.
///
/// The inverse of [`json_from_lua`]. An empty array and an empty object both become an empty
/// table, because Lua has one table type; adapters must not distinguish them.
pub fn lua_from_json<'gc>(ctx: luna::Context<'gc>, value: &serde_json::Value) -> Value<'gc> {
    match value {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map_or_else(|| Value::Number(n.as_f64().unwrap_or(0.0)), Value::Integer),
        serde_json::Value::String(s) => Value::String(luna::String::from_slice(&ctx, s.as_bytes())),
        serde_json::Value::Array(items) => {
            let table = Table::new(&ctx);
            for (index, item) in items.iter().enumerate() {
                // Lua arrays are 1-based.
                table
                    .set(ctx, index as i64 + 1, lua_from_json(ctx, item))
                    .ok();
            }
            Value::Table(table)
        }
        serde_json::Value::Object(fields) => {
            let table = Table::new(&ctx);
            for (key, item) in fields {
                let key = luna::String::from_slice(&ctx, key.as_bytes());
                table.set(ctx, key, lua_from_json(ctx, item)).ok();
            }
            Value::Table(table)
        }
    }
}

/// A Lua table as JSON, dropping anything that cannot be described.
///
/// [`json_from_lua`] refuses a whole table containing a function; a tool spec holds its `run`
/// beside describable fields, so this one drops rather than refuses. `None` when nothing is left.
///
/// # Errors
/// When the value holds itself, nests deeper than [`DEEPEST`], or comes to more than [`NODES`]
/// values or [`BYTES`] of text.
pub fn declaration_from_lua<'gc>(
    ctx: luna::Context<'gc>,
    value: Value<'gc>,
) -> Result<Option<serde_json::Value>, String> {
    Walk::new(ctx, true).value(value)
}

#[cfg(test)]
mod tests {
    use super::{BYTES, NODES};
    use crate::lua::engine::Engine;

    /// What a tool answers, after `body` built a value and handed it to `casper.json.encode`.
    fn encoded(body: &str) -> String {
        let mut engine = Engine::new();
        let source = format!(
            "casper.tool('t', {{ run = function()\n{body}\n\
             local text, why = casper.json.encode(value)\n\
             return text or ('refused: ' .. why) end }})"
        );
        engine.run(&source, "test.lua").expect("declared");
        engine
            .call("t", &serde_json::Value::Null)
            .expect("ran")
            .said
    }

    #[test]
    fn a_value_that_holds_itself_is_refused_by_name() {
        let said = encoded("local value = { a = { b = {} } }; value.a.b.c = value");
        assert_eq!(said, "refused: a table holds itself");

        let mut engine = Engine::new();
        engine
            .run(
                "casper.tool('t', { run = function() local t = { said = 'x' }; t.t = t; return t end })",
                "test.lua",
            )
            .expect("declared");
        let ran = engine.call("t", &serde_json::Value::Null).expect("ran");
        assert!(ran.failed && ran.said.contains("holds itself"), "{ran:?}");
    }

    #[test]
    fn a_table_shared_twice_is_written_twice_and_is_not_a_cycle() {
        let said = encoded("local s = { 1 }; local value = { s, { s } }");
        assert_eq!(said, "[[1],[[1]]]");
    }

    #[test]
    fn a_table_wider_than_the_budget_is_refused() {
        let said = encoded(&format!(
            "local value = {{}}; for at = 1, {} do value[at] = true end",
            NODES + 1
        ));
        assert_eq!(said, format!("refused: more than {NODES} values"));
    }

    #[test]
    fn sharing_that_multiplies_past_either_budget_is_refused() {
        let said = encoded("local value = { 0 }; for _ = 1, 30 do value = { value, value } end");
        assert_eq!(said, format!("refused: more than {NODES} values"));

        let said = encoded(&format!(
            "local s, value = string.rep('x', 1 << 20), {{}}; for at = 1, {} do value[at] = s end",
            (BYTES >> 20) + 1
        ));
        assert_eq!(
            said,
            format!("refused: more than {} MiB of text", BYTES >> 20)
        );
    }

    #[test]
    fn nesting_past_the_deepest_is_refused_and_up_to_it_is_written() {
        let nested = |depth: usize| {
            encoded(&format!(
                "local value = {{}}; for _ = 2, {depth} do value = {{ value }} end"
            ))
        };
        assert!(nested(32).starts_with("[[[["), "{}", nested(32));
        assert_eq!(nested(33), "refused: nested deeper than 32 tables");
    }
}
