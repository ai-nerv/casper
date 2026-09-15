//! The directory lister the family's clients use to find each other, in place of the `io.popen`
//! a sandboxed host may refuse. `fs.dir` is not offered: a client believes whatever directory
//! the host names, so magi answering would send hexe's client looking in magi's directory.

use luna::{Callback, CallbackReturn, Context, Table, Value};

/// Build the `fs` table.
pub fn table<'gc>(ctx: Context<'gc>) -> Table<'gc> {
    let fs = Table::new(&ctx);
    let ls = Callback::from_fn(&ctx, |ctx, _exec, mut stack| {
        let path: Value = stack.consume(ctx)?;
        let Value::String(path) = path else {
            stack.replace(ctx, Value::Nil);
            return Ok(CallbackReturn::Return);
        };
        let path = String::from_utf8_lossy(path.as_bytes()).into_owned();

        let out = Table::new(&ctx);
        if let Ok(entries) = std::fs::read_dir(&path) {
            let mut index = 1_i64;
            for entry in entries.flatten() {
                let record = Table::new(&ctx);
                let name = entry.file_name().to_string_lossy().into_owned();
                record
                    .set(ctx, "name", luna::String::from_slice(&ctx, name.as_bytes()))
                    .ok();

                // Absent rather than zero when unknown: clients sort on it, and zero sorts oldest.
                if let Some(mtime) = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                {
                    record.set(ctx, "mtime", mtime.as_secs() as i64).ok();
                }
                out.set(ctx, index, record).ok();
                index += 1;
            }
        }
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    });
    fs.set(ctx, "ls", ls).ok();
    fs
}
