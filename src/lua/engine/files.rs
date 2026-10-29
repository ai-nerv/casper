use super::*;
use crate::tools::{FileCapability, FileOperation};

pub(super) const RENDERERS: &str = "__casper_file_renderers";

pub(super) fn install<'gc>(
    ctx: luna::Context<'gc>,
    casper: Table<'gc>,
    declared: Rc<RefCell<Declared>>,
) {
    ctx.set_global(RENDERERS, Table::new(&ctx));
    let registrar = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (name, spec): (Value, Value) = stack.consume(ctx)?;
        let (Value::String(name), Value::Table(spec)) = (name, spec) else {
            return Err(raise(
                ctx,
                "casper.file_tool(name, spec): a name and a table",
            ));
        };
        let name = std::str::from_utf8(name.as_bytes())
            .map_err(|_| raise(ctx, "native file name must be UTF-8"))?;
        if name.is_empty()
            || name.len() > 128
            || name.starts_with("mcp_")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(raise(
                ctx,
                "native file tool needs one bounded non-MCP name",
            ));
        }
        let operation = match spec.get_value(ctx, "operation") {
            Value::String(kind) => match kind.as_bytes() {
                b"read" => FileOperation::Read,
                b"edit" => FileOperation::Edit,
                b"write" => FileOperation::Write,
                _ => return Err(raise(ctx, "unsupported native file operation")),
            },
            _ => {
                return Err(raise(
                    ctx,
                    "native file operation must be read, edit or write",
                ));
            }
        };
        for field in ["run", "surface", "screen"] {
            if !matches!(spec.get_value(ctx, field), Value::Nil) {
                return Err(raise(
                    ctx,
                    "native file tools cannot supply an operation callback",
                ));
            }
        }
        let mut card = card(ctx, name, spec)
            .ok_or_else(|| raise(ctx, "native file declaration cannot be described"))?;
        let needs = if operation == FileOperation::Read {
            "read"
        } else {
            "write"
        };
        if card.needs.as_deref().is_some_and(|value| value != needs)
            || card.script && operation != FileOperation::Read
        {
            return Err(raise(
                ctx,
                "native file permission or script capability does not match its operation",
            ));
        }
        card.needs = Some(needs.into());
        card.file_revision = Some(FileCapability {
            version: 1,
            operation,
        });
        if let Value::Table(held) = ctx.get_global_value(TOOLS) {
            held.set(ctx, name, Value::Nil).ok();
        }
        let render = spec.get_value(ctx, "render");
        if !matches!(render, Value::Nil | Value::Function(_)) {
            return Err(raise(ctx, "native render must be a function or absent"));
        }
        if let Value::Table(held) = ctx.get_global_value(RENDERERS) {
            held.set(ctx, name, render).ok();
        }
        let hidden = matches!(spec.get_value(ctx, "hidden"), Value::Boolean(true));
        let mut declared = declared.borrow_mut();
        declared.tools.retain(|previous| previous.name != name);
        declared.native.insert(name.into(), operation);
        if !hidden {
            declared.tools.push(card);
        }
        stack.replace(ctx, ());
        Ok(CallbackReturn::Return)
    });
    casper.set(ctx, "file_tool", registrar).ok();
}

impl Engine {
    pub fn declarations(configuration: Option<serde_json::Value>) -> Self {
        let mut engine = Self::with_configuration(configuration);
        let attempted = Rc::new(Cell::new(false));
        engine.admission = Some(Rc::clone(&attempted));
        engine.lua.enter(|ctx| {
            if let Value::Table(casper) = ctx.get_global_value("casper") {
                let keys: Vec<_> = casper.iter(ctx).map(|(key,_)| key).collect();
                for key in keys {
                    let safe = matches!(key,Value::String(name) if matches!(name.as_bytes(),b"tool"|b"file_tool"|b"json"|b"paint"|b"clients"|b"tapped"));
                    if !safe { let value = blocked(ctx,casper.get_value(ctx,key),&attempted,0); casper.set(ctx,key,value).ok(); }
                }
                ctx.set_global("__stream",casper.get_value(ctx,"stream"));
            }
            for name in ["print","warn"] {
                ctx.set_global(name,Callback::from_fn(&ctx,|ctx,_exec,mut stack| { stack.replace(ctx,()); Ok(CallbackReturn::Return) }));
            }
        });
        engine
    }

    pub fn declarations_clean(&self) -> bool {
        self.admission.as_ref().is_none_or(|value| !value.get())
    }

    pub fn is_declarations(&self) -> bool {
        self.admission.is_some()
    }

    pub fn native(&self, name: &str) -> Option<FileOperation> {
        let declared = self.declared.borrow();
        if !declared.tools.iter().any(|card| card.name == name) {
            return None;
        }
        declared.native.get(name).copied()
    }

    pub fn native_render(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        data: &serde_json::Value,
    ) -> Option<Ran> {
        if self.admission.is_none() {
            return Some(Ran::failed("native render requires a declaration-only VM"));
        }
        let rendered = self.lua.enter(|ctx| {
            let Value::Table(renderers) = ctx.get_global_value(RENDERERS) else {
                return false;
            };
            if !matches!(renderers.get_value(ctx, name), Value::Function(_)) {
                return false;
            }
            ctx.set_global(ARGS, crate::lua::convert::lua_from_json(ctx, args));
            ctx.set_global(
                "__casper_file_data",
                crate::lua::convert::lua_from_json(ctx, data),
            );
            ctx.set_global(RESULT, Value::Nil);
            true
        });
        if !rendered {
            return None;
        }
        if let Err(error) = self.run(
            &format!("{RESULT} = {RENDERERS}[{name:?}]({ARGS}, __casper_file_data)"),
            "native-file-render.lua",
        ) {
            return Some(Ran::failed(error.to_string()));
        }
        let value = self
            .lua
            .enter(|ctx| crate::lua::convert::json_from_lua(ctx, ctx.get_global_value(RESULT)));
        let mut ran = match value {
            Ok(serde_json::Value::String(text)) => Ran::said(text),
            Ok(value) => serde_json::from_value(strip_machine(value))
                .unwrap_or_else(|e| Ran::failed(e.to_string())),
            Err(error) => Ran::failed(error),
        };
        ran.files = Default::default();
        Some(ran)
    }
}

pub(super) fn blocked<'gc>(
    ctx: luna::Context<'gc>,
    value: Value<'gc>,
    attempted: &Rc<Cell<bool>>,
    depth: usize,
) -> Value<'gc> {
    match value {
        Value::Function(_) => {
            let attempted = Rc::clone(attempted);
            Callback::from_fn(&ctx, move |ctx, _exec, _stack| {
                attempted.set(true);
                Err(raise(ctx, "file admission forbids effectful APIs"))
            })
            .into()
        }
        Value::Table(table) if depth < 16 => {
            let safe = Table::new(&ctx);
            for (key, value) in table.iter(ctx) {
                safe.set(ctx, key, blocked(ctx, value, attempted, depth + 1))
                    .ok();
            }
            Value::Table(safe)
        }
        Value::Table(_) | Value::UserData(_) => Value::Nil,
        value => value,
    }
}

pub(super) fn strip_machine(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        for key in ["revision_contract", "observations", "mutation", "absence"] {
            object.remove(key);
        }
    }
    value
}

#[cfg(test)]
mod tests;
