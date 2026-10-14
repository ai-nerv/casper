//! JSON server definitions from Casper's own data directory.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use super::config::{self, Server};

#[derive(Default)]
pub(super) struct Discovered {
    pub servers: BTreeMap<String, Server>,
    pub issues: Vec<String>,
}

pub(super) fn discovered() -> &'static Discovered {
    static FOUND: OnceLock<Discovered> = OnceLock::new();
    FOUND.get_or_init(|| {
        if std::env::var_os(super::NATIVE_ONLY).is_some_and(|value| value == "1") {
            return Discovered::default();
        }
        crate::plugins::mcp_dir().map_or_else(Discovered::default, |path| load(&path))
    })
}

fn load(directory: &Path) -> Discovered {
    let mut found = Discovered::default();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return found,
        Err(_) => {
            found.issues.push(format!(
                "MCP directory {}: cannot read directory",
                directory.display()
            ));
            return found;
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            found
                .issues
                .push("MCP directory: cannot read an entry".into());
            continue;
        };
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            found.issues.push(format!(
                "MCP definition {}: expected a regular JSON file",
                path.display()
            ));
            continue;
        }
        files.push(path);
    }
    files.sort();
    for path in files {
        match definition(&path) {
            Ok(servers) => found.servers.extend(servers),
            Err(why) => found
                .issues
                .push(format!("MCP definition {}: {why}", path.display())),
        }
    }
    found
}

fn definition(path: &Path) -> Result<BTreeMap<String, Server>, &'static str> {
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("invalid server filename")?;
    let file = std::fs::File::open(path).map_err(|_| "cannot read definition")?;
    let mut source = String::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|_| "cannot read definition")?;
    if source.len() > 4 * 1024 * 1024 {
        return Err("definition exceeds 4 MiB");
    }
    let server: serde_json::Value =
        serde_json::from_str(&source).map_err(|_| "invalid JSON definition")?;
    config::parse(Some(&serde_json::json!({name:server})))
        .map_err(|_| "invalid server definition; use a valid alias filename and command or url with native MCP settings")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn discovers_only_regular_json_definitions_in_the_selected_folder() {
        let root = Scratch::new("mcp-dir", "definitions");
        let directory = root.join("mcp");
        std::fs::create_dir(&directory).expect("directory");
        std::fs::write(
            directory.join("stdio.json"),
            r#"{"command":"arbitrary-server","args":["--stdio"]}"#,
        )
        .expect("stdio");
        std::fs::write(
            directory.join("remote.json"),
            r#"{"url":"http://localhost:1234/mcp","headers":{"Authorization":"Bearer fixture"}}"#,
        )
        .expect("HTTP");
        std::fs::write(directory.join("ignored.toml"), "invalid input").expect("ignored file");
        std::fs::create_dir(directory.join("nested")).expect("nested directory");
        std::fs::write(
            directory.join("nested/hidden.json"),
            r#"{"command":"never-launch"}"#,
        )
        .expect("nested definition");
        let found = load(&directory);
        assert!(found.issues.is_empty(), "{:?}", found.issues);
        assert_eq!(found.servers.len(), 2);
        assert_eq!(found.servers["stdio"].args, ["--stdio"]);
        assert_eq!(
            found.servers["remote"].headers["Authorization"],
            "Bearer fixture"
        );
        assert!(load(&root.join("missing")).servers.is_empty());
    }

    #[test]
    fn invalid_files_are_isolated_and_diagnostics_do_not_include_secrets() {
        let root = Scratch::new("mcp-dir", "invalid");
        std::fs::write(root.join("good.json"), r#"{"command":"server"}"#).expect("good definition");
        std::fs::write(
            root.join("bad.json"),
            r#"{"command":"SECRET","url":"SECRET"}"#,
        )
        .expect("bad definition");
        std::fs::write(root.join("syntax.json"), "SECRET").expect("invalid JSON");
        let found = load(&root);
        assert_eq!(found.servers.len(), 1);
        assert_eq!(found.issues.len(), 2);
        assert!(!found.issues.join("\n").contains("SECRET"));
    }

    #[test]
    fn symlinked_definitions_are_not_read() {
        let root = Scratch::new("mcp-dir", "symlinks");
        let directory = root.join("mcp");
        std::fs::create_dir(&directory).expect("directory");
        std::fs::write(root.join("outside.json"), r#"{"command":"outside-server"}"#)
            .expect("outside definition");
        std::os::unix::fs::symlink(root.join("outside.json"), directory.join("linked.json"))
            .expect("symlink");
        let found = load(&directory);
        assert!(found.servers.is_empty());
        assert_eq!(found.issues.len(), 1);
    }
}
