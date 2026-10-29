//! External MCP sessions, discovered tool cards, and calls through trusted server settings.

mod admission;
pub mod authentication;
mod cancellation;
mod config;
mod directory;
mod http;
mod lifecycle;
pub mod oauth;
mod observing;
mod pooling;
pub mod read_broker;
mod stdio;

#[cfg(test)]
#[path = "mcp_client/images.rs"]
mod images;

pub(crate) const NATIVE_ONLY: &str = "CASPER_MCP_NATIVE";

/// The most one message from a server may hold: one stdio line, one SSE event, or one JSON body.
const REPLY: usize = 4 * 1024 * 1024;

pub(crate) fn server_input() -> impl tokio::io::AsyncRead + Unpin {
    stdio::Capped::new(tokio::io::stdin(), REPLY, Arc::new(AtomicBool::new(false)))
}

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientHandler, ServiceExt, model::*};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::tools::{Card, Ran};
use config::Server;

/// One pool for the process: `serve` answers each connection on a thread of its own, and a
/// session outlives the call that opened it.
static HUB: Mutex<Option<Hub>> = Mutex::new(None);

struct Hub {
    runtime: tokio::runtime::Runtime,
    scope: Arc<lifecycle::Scope>,
    sessions: BTreeMap<String, Arc<Session>>,
}

struct Session {
    stamp: String,
    auth_key: Option<String>,
    auth: Option<Arc<oauth::Access>>,
    service: RunningService<RoleClient, Client>,
    catalog: BTreeMap<String, Target>,
    cards: Vec<Card>,
    /// Set once a server wrote a message past [`REPLY`]; the session is not used again.
    over: Arc<AtomicBool>,
    server: Mutex<Option<stdio::Served>>,
    stopped: AtomicBool,
    root: std::path::PathBuf,
    sequence: std::sync::atomic::AtomicU64,
    read_slots: tokio::sync::Semaphore,
    trace: observing::Trace,
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("casper", env!("CARGO_PKG_VERSION")),
        )
    }
}

enum Target {
    Tool(String),
    NativeRead(String),
    Resources,
    Prompts,
}

fn servers(configuration: Option<&Value>) -> Result<BTreeMap<String, Server>, String> {
    let mut servers = directory::discovered().servers.clone();
    for issue in &directory::discovered().issues {
        eprintln!("casper: {issue}");
    }
    servers.extend(config::parse(configuration)?);
    Ok(servers)
}

/// End a session, waiting for it when no call still holds it.
fn close(runtime: &tokio::runtime::Runtime, session: Arc<Session>) {
    if let Some(auth) = &session.auth {
        auth.stop();
    }
    session.trace.closing();
    match Arc::try_unwrap(session) {
        Ok(session) => {
            let _ = runtime.block_on(session.service.cancel());
        }
        Err(shared) => shared.service.cancellation_token().cancel(),
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        for (_, session) in std::mem::take(&mut self.sessions) {
            close(&self.runtime, session);
        }
        self.runtime
            .block_on(async { tokio::time::sleep(Duration::from_millis(20)).await });
    }
}

impl Session {
    async fn discover(&mut self, alias: &str, server: &Server) -> Result<(), String> {
        let info = self
            .service
            .peer_info()
            .ok_or("MCP peer information missing")?;
        let has_tools = info.capabilities.tools.is_some();
        let has_resources = info.capabilities.resources.is_some();
        let has_prompts = info.capabilities.prompts.is_some();
        if has_tools {
            let tools = self
                .service
                .list_all_tools()
                .await
                .map_err(|why| format!("tools/list: {why}"))?;
            for tool in tools {
                if server.permits(&tool.name) {
                    let native = if server.command.is_some() {
                        read_broker::negotiated(
                            tool.meta
                                .as_ref()
                                .and_then(|meta| meta.get(read_broker::KEY)),
                            &self.root,
                        )?
                    } else {
                        false
                    };
                    if native && !matches!(server.needs.as_str(), "read" | "run") {
                        return Err("native read broker requires read or command permission".into());
                    }
                    self.insert(
                        Card {
                            name: tool_name(alias, &tool.name),
                            description: format!(
                                "[MCP {alias}] {}",
                                tool.description.as_deref().unwrap_or(&tool.name)
                            ),
                            parameters: Value::Object((*tool.input_schema).clone()),
                            needs: Some(server.needs.clone()),
                            deferred: false,
                            group: format!("mcp:{alias}"),
                            script: false,
                            file_revision: None,
                        },
                        if native {
                            Target::NativeRead(tool.name.into_owned())
                        } else {
                            Target::Tool(tool.name.into_owned())
                        },
                    )?;
                }
            }
        }
        if has_resources && server.tools.is_none() {
            self.insert(Card {
                name: format!("mcp_{alias}_resources"),
                description: format!("List or read MCP resources and resource templates from {alias}"),
                parameters: json!({"type":"object","properties":{"action":{"type":"string","enum":["list","templates","read"]},"uri":{"type":"string"}},"required":["action"],"additionalProperties":false}),
                needs: Some(server.needs.clone()), deferred: false, group: format!("mcp:{alias}"), script: false, file_revision: None,
            }, Target::Resources)?;
        }
        if has_prompts && server.tools.is_none() {
            self.insert(Card {
                name: format!("mcp_{alias}_prompts"),
                description: format!("List or retrieve MCP prompt templates from {alias}"),
                parameters: json!({"type":"object","properties":{"action":{"type":"string","enum":["list","get"]},"name":{"type":"string"},"arguments":{"type":"object","additionalProperties":{"type":"string"}}},"required":["action"],"additionalProperties":false}),
                needs: Some(server.needs.clone()), deferred: false, group: format!("mcp:{alias}"), script: false, file_revision: None,
            }, Target::Prompts)?;
        }
        Ok(())
    }

    fn insert(&mut self, card: Card, target: Target) -> Result<(), String> {
        if self.catalog.insert(card.name.clone(), target).is_some() {
            return Err(format!("duplicate MCP tool name: {}", card.name));
        }
        self.cards.push(card);
        Ok(())
    }

    async fn call(&self, name: &str, args: &Value, timeout: Duration) -> Result<Ran, String> {
        let target = self
            .catalog
            .get(name)
            .ok_or_else(|| format!("no such configured MCP tool: {name}"))?;
        let fields = if args.is_null() {
            serde_json::Map::new()
        } else {
            args.as_object()
                .cloned()
                .ok_or("MCP arguments must be an object")?
        };
        if let Target::Tool(remote) | Target::NativeRead(remote) = target {
            let began = std::time::Instant::now();
            let mut params = CallToolRequestParams::new(remote.clone()).with_arguments(fields);
            let _slot = if matches!(target, Target::NativeRead(_)) {
                let prepare = async {
                    let slot = self
                        .read_slots
                        .acquire()
                        .await
                        .map_err(|_| "native read broker stopping")?;
                    let sequence = self
                        .sequence
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                            value.checked_add(1)
                        })
                        .map_err(|_| "native read broker sequence exhausted")?
                        + 1;
                    let data = read_broker::acquire(&self.root, args).await?;
                    let envelope = read_broker::Envelope::new(
                        sequence,
                        &self.root,
                        remote,
                        args.clone(),
                        data,
                    );
                    let value = serde_json::to_value(envelope).map_err(|e| e.to_string())?;
                    if value.to_string().len() > read_broker::LIMIT {
                        return Err("native read broker envelope exceeds its bound".into());
                    }
                    let mut meta = RequestMetaObject::new();
                    meta.insert(read_broker::KEY.into(), value);
                    Ok::<_, String>((slot, meta))
                };
                let (slot, meta) =
                    tokio::time::timeout(timeout, prepare).await.map_err(|_| {
                        "native read broker acquisition exceeded its deadline; not retried"
                            .to_owned()
                    })??;
                params.meta = Some(meta);
                Some(slot)
            } else {
                None
            };
            let remaining = timeout
                .checked_sub(began.elapsed())
                .filter(|duration| !duration.is_zero())
                .ok_or("MCP call deadline expired before dispatch; not retried")?;
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
            let result = self
                .service
                .send_request_with_option(
                    request,
                    rmcp::service::PeerRequestOptions::with_timeout(remaining),
                )
                .await
                .map_err(|why| why.to_string())?
                .await_response()
                .await
                .map_err(|why| {
                    format!(
                        "MCP call: {why}; not retried; remote effects may already have occurred"
                    )
                })?;
            if let ServerResult::CallToolResult(result) = result {
                return attachment_result(
                    serde_json::to_value(result).map_err(|why| why.to_string())?,
                );
            }
            return Err("MCP tool returned an unexpected response".into());
        }
        let operation = async {
            let value = match (target, args.get("action").and_then(Value::as_str)) {
                (Target::Resources, Some("list")) => serde_json::to_value(
                    self.service
                        .list_all_resources()
                        .await
                        .map_err(|why| why.to_string())?,
                ),
                (Target::Resources, Some("templates")) => serde_json::to_value(
                    self.service
                        .list_all_resource_templates()
                        .await
                        .map_err(|why| why.to_string())?,
                ),
                (Target::Resources, Some("read")) => serde_json::to_value(
                    self.service
                        .read_resource(ReadResourceRequestParams::new(required(args, "uri")?))
                        .await
                        .map_err(|why| why.to_string())?,
                ),
                (Target::Prompts, Some("list")) => serde_json::to_value(
                    self.service
                        .list_all_prompts()
                        .await
                        .map_err(|why| why.to_string())?,
                ),
                (Target::Prompts, Some("get")) => {
                    let arguments = args
                        .get("arguments")
                        .map(|value| serde_json::from_value(value.clone()))
                        .transpose()
                        .map_err(|why| format!("prompt arguments: {why}"))?;
                    let mut params = GetPromptRequestParams::new(required(args, "name")?);
                    params.arguments = arguments;
                    serde_json::to_value(
                        self.service
                            .get_prompt(params)
                            .await
                            .map_err(|why| why.to_string())?,
                    )
                }
                _ => return Err("unknown MCP resource/prompt action".into()),
            }
            .map_err(|why| why.to_string())?;
            attachment_result(value)
        };
        tokio::time::timeout(timeout, operation)
            .await
            .map_err(|_| "MCP request timed out; not retried".to_owned())?
    }
}

fn attachment_result(mut value: Value) -> Result<Ran, String> {
    fn extract(value: &mut Value, images: &mut Vec<crate::images::Image>, rejected: &mut bool) {
        match value {
            Value::Object(fields) => {
                let media = fields
                    .get("mimeType")
                    .or_else(|| fields.get("media_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let data = fields
                    .get("data")
                    .or_else(|| fields.get("blob"))
                    .and_then(Value::as_str);
                if (media.starts_with("image/")
                    || fields.get("type").and_then(Value::as_str) == Some("image"))
                    && let Some(data) = data
                {
                    let result = if images.len() < crate::images::MAX_IMAGES {
                        crate::images::encoded(data, media)
                    } else {
                        Err("at most four image attachments per result".into())
                    };
                    let note = match result {
                        Ok(image) => {
                            images.push(image);
                            let source = fields.get("uri").and_then(Value::as_str).unwrap_or("");
                            format!(
                                "[Image attachment {} {source}; resized for inline vision]",
                                images.len()
                            )
                        }
                        Err(why) => {
                            *rejected = true;
                            format!("[Image omitted: {why}]")
                        }
                    };
                    *value = json!({"type":"text", "text":note});
                } else {
                    for child in fields.values_mut() {
                        extract(child, images, rejected);
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    extract(child, images, rejected);
                }
            }
            _ => {}
        }
    }
    let failed = value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (mut images, mut rejected) = (Vec::new(), false);
    extract(&mut value, &mut images, &mut rejected);
    Ok(Ran {
        said: serde_json::to_string(&value).map_err(|why| why.to_string())?,
        images,
        failed: failed || rejected,
        ..Ran::default()
    })
}

fn required(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("MCP action needs {key}"))
}

/// Why a session ended, when it was a message past [`REPLY`].
fn oversized(over: &AtomicBool) -> Option<String> {
    over.load(Ordering::Relaxed).then(|| {
        format!(
            "MCP server wrote a message over {} MiB; the session was closed and nothing was retried",
            REPLY / (1024 * 1024)
        )
    })
}

/// The server alias a namespaced name belongs to. An alias holds no `__` and neither starts nor
/// ends with `_`, so the first `__` after `mcp__` ends it and no alias is a prefix of another's.
fn alias(name: &str) -> Option<&str> {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let (alias, remote) = rest.split_once("__")?;
        return (!alias.is_empty() && !remote.is_empty()).then_some(alias);
    }
    let rest = name.strip_prefix("mcp_")?;
    rest.strip_suffix("_resources")
        .or_else(|| rest.strip_suffix("_prompts"))
        .filter(|alias| !alias.is_empty())
}

fn tool_name(alias: &str, remote: &str) -> String {
    let name = format!("mcp__{alias}__{remote}");
    if name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        name
    } else {
        format!("mcp__{alias}__{:x}", Sha256::digest(remote.as_bytes()))[..(alias.len() + 39)]
            .to_owned()
    }
}

fn using<T>(operation: impl FnOnce(&mut Hub) -> Result<T, String>) -> Result<T, String> {
    let scope = lifecycle::enter()?;
    let caller = crate::cancelling::capture();
    let mut slot = cancellation::lock(&HUB, &caller, || scope.check())?;
    cancellation::check(&caller)?;
    scope.check()?;
    if slot.is_none() {
        *slot = Some(Hub::new()?);
    }
    slot.as_mut()
        .ok_or("MCP runtime unavailable")?
        .admit(&caller, &scope, operation)
}

/// Discover configured external tools, resources and prompts.
pub fn cards(configuration: Option<&Value>) -> Result<Vec<Card>, String> {
    let servers = servers(configuration)?;
    if servers.is_empty() {
        return Ok(Vec::new());
    }
    using(|hub| {
        let mut cards = Vec::new();
        for (name, server) in servers.iter().filter(|(_, server)| !server.off) {
            match hub.session(name, server) {
                Ok(session) => cards.extend(session.cards.clone()),
                Err(why) => eprintln!("casper: MCP server {name}: {why}"),
            }
        }
        Ok(cards)
    })
}

/// List configured servers, connection status, and discovered tool pages.
pub fn manual(
    configuration: Option<&Value>,
    path: &str,
    tools: Option<&Value>,
    issues: Option<&Value>,
    catalog: Option<&Value>,
) -> Ran {
    let result = (|| {
        let servers = servers(configuration)?;
        let rest = path.strip_prefix("mcp/").unwrap_or("");
        let (selected, tool) = rest.split_once('/').unwrap_or((rest, ""));
        if !selected.is_empty() && !servers.contains_key(selected) {
            return Err(
                "no such MCP server; tools({path: \"mcp\"}) lists configured servers".into(),
            );
        }
        using(|hub| {
            let mut lines = vec!["MCP support is available through Casper.".to_owned()];
            lines.extend(directory::discovered().issues.iter().cloned());
            if let Some(issues) = issues.and_then(Value::as_array) {
                lines.extend(issues.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            if servers.is_empty() {
                lines.push("No MCP servers are configured. Put a server JSON definition in $XDG_DATA_HOME/casper/mcp/ (default ~/.local/share/casper/mcp/).".into());
            }
            for (name, server) in &servers {
                if !selected.is_empty() && selected != name {
                    continue;
                }
                if server.off {
                    lines.push(format!("mcp/{name} -- disabled"));
                    continue;
                }
                if let Some(catalog) = catalog.and_then(Value::as_array) {
                    let cards: Vec<Card> = catalog
                        .iter()
                        .filter_map(|card| serde_json::from_value(card.clone()).ok())
                        .filter(|card: &Card| alias(&card.name) == Some(name.as_str()))
                        .filter(|card| {
                            let settings = tools.and_then(|tools| tools.get(&card.name));
                            !["off", "hidden"].iter().any(|flag| {
                                settings
                                    .and_then(|settings| settings.get(flag))
                                    .and_then(Value::as_bool)
                                    == Some(true)
                            })
                        })
                        .collect();
                    lines.push(format!("mcp/{name} -- {} tools discovered at session startup; availability is checked when called", cards.len()));
                    for card in &cards {
                        if tool.is_empty() {
                            lines.push(format!("  {} -- {}", card.name, card.description));
                        } else if tool == card.name {
                            lines.push(format!("{}\n{}\nArguments: {}\nCall this name directly as a model tool, not a shell command.", card.name, card.description, card.parameters));
                        }
                    }
                    if !tool.is_empty() && !cards.iter().any(|card| card.name == tool) {
                        return Err("no such available MCP tool".into());
                    }
                    continue;
                }
                let session = match hub.session(name, server) {
                    Ok(session) => session,
                    Err(why) => {
                        eprintln!("casper: MCP server {name}: {why}");
                        lines.push(format!(
                            "mcp/{name} -- connection/discovery failed; see Casper diagnostics"
                        ));
                        continue;
                    }
                };
                let cards: Vec<_> = session
                    .cards
                    .iter()
                    .filter(|card| {
                        let settings = tools.and_then(|tools| tools.get(&card.name));
                        !["off", "hidden"].iter().any(|flag| {
                            settings
                                .and_then(|settings| settings.get(flag))
                                .and_then(Value::as_bool)
                                == Some(true)
                        })
                    })
                    .collect();
                lines.push(format!(
                    "mcp/{name} -- connected, {} available tools",
                    cards.len()
                ));
                if !tool.is_empty() {
                    let card = cards
                        .iter()
                        .find(|card| card.name == tool)
                        .ok_or("no such available MCP tool")?;
                    lines.push(format!("{}\n{}\nArguments: {}\nCall this name directly as a model tool, not a shell command.", card.name, card.description, card.parameters));
                } else {
                    for card in cards {
                        lines.push(format!("  {} -- {}", card.name, card.description));
                    }
                }
            }
            Ok(lines.join("\n"))
        })
    })();
    match result {
        Ok(said) => Ran::said(said),
        Err(why) => Ran {
            said: why,
            failed: true,
            ..Ran::default()
        },
    }
}

/// Run a discovered namespaced operation without replaying failed remote requests.
pub fn call(configuration: Option<&Value>, name: &str, args: &Value) -> Ran {
    let caller = crate::cancelling::capture();
    let result = (|| {
        if caller.abandoned() {
            return Err(cancellation::DISCONNECTED.into());
        }
        let servers = servers(configuration)?;
        let unknown = || format!("no such configured MCP tool: {name}");
        let (owner, server) = alias(name)
            .and_then(|owner| servers.get_key_value(owner))
            .filter(|(_, server)| !server.off)
            .ok_or_else(unknown)?;
        // The pool is held only to find the session: a slow call keeps no other caller waiting.
        let (runtime, scope, session) = using(|hub| {
            Ok((
                hub.runtime.handle().clone(),
                hub.scope.clone(),
                hub.session(owner, server)?,
            ))
        })?;
        let result = runtime
            .block_on(scope.run(cancellation::run(
                &caller,
                session.call(name, args, Duration::from_secs(server.timeout_secs)),
            )))
            .map_err(|why| match oversized(&session.over) {
                Some(said) => {
                    session.service.cancellation_token().cancel();
                    said
                }
                None => why,
            });
        if caller.abandoned() {
            session.stop();
            return Err(cancellation::DISCONNECTED.into());
        }
        result
    })();
    result.unwrap_or_else(Ran::failed)
}

/// Close pooled sessions before the process exits.
pub fn shutdown() {
    let _closing = lifecycle::closing();
    let pool = HUB.lock().unwrap_or_else(PoisonError::into_inner).take();
    drop(pool);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_belongs_to_the_one_alias_that_made_it() {
        for (owner, remote) in [
            ("a", "read"),
            ("a", "b__read"),
            ("a_b", "read"),
            ("x1", "__init__"),
            ("blender", "get scene info"),
        ] {
            assert_eq!(alias(&tool_name(owner, remote)), Some(owner), "{remote}");
        }
        assert_eq!(alias("mcp_a_b_resources"), Some("a_b"));
        assert_eq!(alias("mcp_a_prompts"), Some("a"));
        for stray in [
            "mcp__a",
            "mcp__a__",
            "mcp____read",
            "mcp_resources",
            "mcp__resources",
            "mcp_a_b",
            "read",
        ] {
            assert_eq!(alias(stray), None, "{stray}");
        }
    }

    #[test]
    fn no_alias_can_be_the_prefix_of_another_aliases_tools() {
        for name in ["a__b", "a_", "_a", "__", "a___b"] {
            let refused = config::parse(Some(&json!({name: {"command": "server"}}))).err();
            assert!(
                refused.is_some_and(|why| why.contains("single underscores")),
                "{name}"
            );
        }
        let both = json!({"a": {"command": "server"}, "a_b": {"command": "server"}});
        assert_eq!(
            config::parse(Some(&both)).map(|servers| servers.len()),
            Ok(2)
        );
    }
}
