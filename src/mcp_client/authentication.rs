use super::{HUB, close, oauth, servers};
use serde_json::{Value, json};
use std::sync::TryLockError;

/// Authenticate, inspect or invalidate credentials for a configured server.
pub fn command(
    verb: &str,
    args: &[String],
    configuration: Option<&Value>,
) -> Result<Value, String> {
    let name = args
        .first()
        .ok_or("MCP authentication command needs a server name")?;
    if args.len() > 2
        || name.starts_with('-')
        || args
            .get(1)
            .is_some_and(|arg| !matches!(arg.as_str(), "--json" | "--cbor"))
        || !matches!(verb, "mcp-auth" | "mcp-status" | "mcp-logout")
    {
        return Err("expected mcp-auth|mcp-status|mcp-logout SERVER [--json|--cbor]".into());
    }
    let configured = servers(configuration)?;
    let server = configured.get(name).ok_or("unknown MCP server")?;
    let auth = server
        .auth
        .as_ref()
        .ok_or("MCP server has no OAuth configuration")?;
    let binding = auth
        .bind(server.url.as_deref().ok_or("MCP OAuth requires HTTP")?)
        .map_err(|error| error.to_string())?;
    let key = binding.key();
    if verb == "mcp-auth" {
        let generation = login(
            auth,
            server.url.as_deref().ok_or("MCP OAuth requires HTTP")?,
        )?;
        invalidate(&key);
        return Ok(json!({"authenticated":true,"generation":generation}));
    }
    let store = oauth::Store::open(binding).map_err(|error| error.to_string())?;
    if verb == "mcp-logout" {
        let generation = store.logout().map_err(|error| error.to_string())?;
        invalidate(&key);
        return Ok(json!({"authenticated":false,"generation":generation}));
    }
    let snapshot = store.snapshot().map_err(|error| error.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock unavailable")?
        .as_secs();
    Ok(json!({
        "authenticated":snapshot.credential.as_ref().is_some_and(|value| value.fresh(now)),
        "credential_present":snapshot.credential.is_some(),
        "refresh_available":snapshot.credential.as_ref().is_some_and(|value| value.refresh_token.is_some()),
        "expires_at":snapshot.credential.as_ref().and_then(|value| value.expires_at),
        "generation":snapshot.generation,
    }))
}

fn login(config: &oauth::Config, endpoint: &str) -> Result<u64, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "MCP OAuth runtime unavailable")?;
    let result = runtime.block_on(async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| "MCP OAuth cancellation unavailable")?;
        let mut terminate = signal(SignalKind::terminate()).map_err(|_| "MCP OAuth cancellation unavailable")?;
        tokio::select! {
            _ = interrupt.recv() => Err("MCP OAuth cancelled".into()),
            _ = terminate.recv() => Err("MCP OAuth cancelled".into()),
            result = oauth::login(config, endpoint, |url| async move {
                use tokio::io::AsyncWriteExt;
                tokio::io::stderr().write_all(format!("Open this URL in your browser to authorize this MCP server:\n{url}\n").as_bytes()).await.map_err(|_| oauth::Error::Callback)
            }) => result.map_err(|error| error.to_string()),
        }
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

fn invalidate(key: &str) {
    oauth::Access::stop_key(key);
    let mut hub = match HUB.try_lock() {
        Ok(hub) => hub,
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    if let Some(hub) = hub.as_mut() {
        let names: Vec<_> = hub
            .sessions
            .iter()
            .filter(|(_, session)| session.auth_key.as_deref() == Some(key))
            .map(|(name, _)| name.clone())
            .collect();
        for name in names {
            if let Some(session) = hub.sessions.remove(&name) {
                close(&hub.runtime, session);
            }
        }
    }
}
