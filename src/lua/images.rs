//! Image-file reads through a bounded jailed reader.

use luna::{Callback, CallbackReturn, Value};

pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let path: Value = stack.consume(ctx)?;
        let Value::String(path) = path else {
            return Err(error(ctx, "casper.image needs a file path"));
        };
        let path = String::from_utf8_lossy(path.as_bytes());
        let image = crate::images::jailed_file(&path).map_err(|e| error(ctx, &e))?;
        let value = serde_json::to_value(image).map_err(|e| error(ctx, &e.to_string()))?;
        stack.replace(ctx, crate::lua::convert::lua_from_json(ctx, &value));
        Ok(CallbackReturn::Return)
    })
}

fn error<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}
