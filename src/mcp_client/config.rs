use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Server {
    pub command: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub excluded_tools: Vec<String>,
    #[serde(default = "permission")]
    pub needs: String,
    #[serde(default = "deadline")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub off: bool,
}

fn permission() -> String {
    "run".into()
}

fn deadline() -> u64 {
    60
}

pub(super) fn parse(value: Option<&Value>) -> Result<BTreeMap<String, Server>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(BTreeMap::new());
    };
    let servers: BTreeMap<String, Server> = serde_json::from_value(value.clone())
        .map_err(|why| format!("invalid mcp_servers: {why}"))?;
    for (name, server) in &servers {
        // Words joined by single underscores: `mcp__{name}__` then ends where the name does.
        if name.is_empty()
            || name.len() > 20
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name.contains("__")
            || name.starts_with('_')
            || name.ends_with('_')
        {
            return Err(format!(
                "MCP server {name:?}: name must be 1–20 ASCII letters and digits, words joined by single underscores"
            ));
        }
        if server.command.is_some() == server.url.is_some() {
            return Err(format!(
                "MCP server {name}: set exactly one of command or url"
            ));
        }
        if !matches!(server.needs.as_str(), "read" | "write" | "run" | "reach") {
            return Err(format!(
                "MCP server {name}: needs must be read, write, run or reach"
            ));
        }
        if !(1..=300).contains(&server.timeout_secs) {
            return Err(format!("MCP server {name}: timeout_secs must be 1–300"));
        }
        if server
            .command
            .as_ref()
            .is_some_and(|command| command.trim().is_empty())
        {
            return Err(format!("MCP server {name}: command is empty"));
        }
        if let Some(uri) = &server.url {
            let parsed = url::Url::parse(uri).map_err(|why| format!("MCP server {name}: {why}"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err(format!(
                    "MCP server {name}: expected an HTTP(S) URL without credentials or fragment"
                ));
            }
            if !server.args.is_empty() || !server.env.is_empty() {
                return Err(format!(
                    "MCP server {name}: args/env require a stdio command"
                ));
            }
        } else if !server.headers.is_empty() {
            return Err(format!("MCP server {name}: headers require an HTTP URL"));
        }
    }
    Ok(servers)
}

impl Server {
    pub fn permits(&self, tool: &str) -> bool {
        !self.excluded_tools.iter().any(|excluded| excluded == tool)
            && self
                .tools
                .as_ref()
                .is_none_or(|tools| tools.iter().any(|one| one == tool))
    }
}
