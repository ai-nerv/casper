use luna::{Callback, CallbackReturn, Value};

pub fn table(ctx: luna::Context<'_>, configuration: Option<serde_json::Value>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let args: Value = stack.consume(ctx)?;
        let args =
            crate::lua::convert::json_from_lua(ctx, args, 0).unwrap_or(serde_json::Value::Null);
        let result = match crate::web::run(args, configuration.as_ref()) {
            Ok(value) => {
                serde_json::json!({"said": value.to_string(), "brief": "web sources", "keep": true})
            }
            Err(why) => serde_json::json!({"said": why, "failed": true}),
        };
        stack.replace(ctx, crate::lua::convert::lua_from_json(ctx, &result));
        Ok(CallbackReturn::Return)
    })
}
