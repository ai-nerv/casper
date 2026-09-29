use luna::{Callback, CallbackReturn, Value};

pub fn table(ctx: luna::Context<'_>, configuration: Option<serde_json::Value>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let args: Value = stack.consume(ctx)?;
        let args =
            crate::lua::convert::json_from_lua(ctx, args, 0).unwrap_or(serde_json::Value::Null);
        let ran = crate::browser::run(args, configuration.as_ref())
            .unwrap_or_else(crate::tools::Ran::failed);
        let result = serde_json::to_value(ran)
            .unwrap_or_else(|why| serde_json::json!({"failed":true,"said":why.to_string()}));
        stack.replace(ctx, crate::lua::convert::lua_from_json(ctx, &result));
        Ok(CallbackReturn::Return)
    })
}
