//! Resource-bound OAuth configuration and private credential state.

mod access;
mod callback;
mod cancellation;
mod challenge;
mod config;
mod control;
mod discovery;
mod login;
mod pkce;
mod refresh;
mod store;
mod token;

pub(super) use access::Access;
pub use config::{Binding, Config};
pub(super) use control::client;
pub use discovery::{Discovery, discover};
pub use login::login;
pub use store::{Credential, Snapshot, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid MCP OAuth configuration")]
    Configuration,
    #[error("MCP OAuth credential store is unsafe or unavailable")]
    Store,
    #[error("MCP OAuth credential lock timed out")]
    Busy,
    #[error("MCP OAuth refresh temporarily unavailable")]
    Backoff,
    #[error("MCP OAuth operation cancelled")]
    Cancelled,
    #[error("MCP OAuth credentials changed; stale update rejected")]
    Changed,
    #[error("invalid MCP OAuth credential response")]
    Credential,
    #[error("authentication_required")]
    Required,
    #[error("MCP OAuth network request refused or failed")]
    Network,
    #[error("MCP OAuth operation timed out")]
    Timeout,
    #[error("invalid MCP OAuth metadata")]
    Metadata,
    #[error("MCP OAuth callback refused or unavailable")]
    Callback,
}

pub(crate) fn random() -> Result<String, Error> {
    use std::io::Read;
    let mut bytes = [0; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| Error::Store)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
