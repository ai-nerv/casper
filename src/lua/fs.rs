//! The directory lister the family's clients use to find each other, in place of the `io.popen`
//! a sandboxed host may refuse. `fs.dir` is not offered: a client believes whatever directory
//! the host names, so magi answering would send hexe's client looking in magi's directory.
//!
//! Only the family's own socket directories are listed, the same set `stream` may dial.

use luna::{Callback, CallbackReturn, Context, Table, Value};
use std::path::{Path, PathBuf};

/// The canonical directory to list when `path` is inside one of `roots`.
fn listable_in(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    roots
        .iter()
        .any(|root| real.starts_with(root))
        .then_some(real)
}

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
        let Some(real) = listable_in(Path::new(&path), &super::stream::roots()) else {
            stack.replace(
                ctx,
                (
                    Value::Nil,
                    "only a family program's own socket directory is this VM's to list",
                ),
            );
            return Ok(CallbackReturn::Return);
        };

        let out = Table::new(&ctx);
        if let Ok(entries) = std::fs::read_dir(&real) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn only_a_family_directory_is_listable() {
        let dir = Scratch::new("fs", "family");
        let root = std::fs::canonicalize(&*dir).expect("canonical");
        std::fs::create_dir_all(root.join("magi/sub")).expect("family dir");
        std::os::unix::fs::symlink(&root, root.join("magi/up")).expect("link");
        let roots = [root.join("magi")];
        assert!(listable_in(&root.join("magi/sub"), &roots).is_some());
        for path in [root.clone(), root.join("magi/up"), root.join("magi/../")] {
            assert_eq!(listable_in(&path, &roots), None, "{}", path.display());
        }
    }

    #[test]
    fn a_config_cannot_list_an_arbitrary_directory() {
        let mut engine = crate::lua::engine::Engine::new();
        engine
            .run(
                "local got, why = casper.fs.ls('/etc')\ncasper.answer = tostring(got) .. ' ' .. why",
                "probe.lua",
            )
            .expect("runs");
        engine.harvest();
        let said = engine.setting("answer").unwrap_or_default();
        assert!(
            said.as_str().is_some_and(|s| s.starts_with("nil ")),
            "{said}"
        );
    }
}
