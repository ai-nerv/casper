use super::{Credential, Error, config};
use serde::Deserialize;

#[derive(Default)]
enum Optional {
    #[default]
    Missing,
    Present(Option<String>),
}

impl<'de> Deserialize<'de> for Optional {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<String>::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    error: Optional,
    access_token: String,
    token_type: String,
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Optional,
    scope: Option<String>,
}

pub(super) fn credential(
    bytes: &[u8],
    previous: Option<&Credential>,
    requested: &[String],
    now: u64,
) -> Result<Credential, Error> {
    if bytes.len() > 65536 {
        return Err(Error::Credential);
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| Error::Credential)?;
    if !matches!(reply.error, Optional::Missing)
        || !reply.token_type.eq_ignore_ascii_case("Bearer")
        || reply.expires_in == Some(0)
    {
        return Err(Error::Credential);
    }
    let refresh_token = match reply.refresh_token {
        Optional::Missing => previous.and_then(|old| old.refresh_token.clone()),
        Optional::Present(value) => value,
    };
    let scopes = match reply.scope {
        Some(scopes) => scopes.split(' ').map(str::to_owned).collect(),
        None if !requested.is_empty() => requested.to_vec(),
        None => previous.map_or_else(Vec::new, |old| old.scopes.clone()),
    };
    if scopes.len() > 32 || !scopes.iter().all(|scope| config::scope(scope)) {
        return Err(Error::Credential);
    }
    let expires_at = reply
        .expires_in
        .map(|seconds| now.checked_add(seconds).ok_or(Error::Credential))
        .transpose()?;
    let credential = Credential {
        access_token: reply.access_token,
        refresh_token,
        expires_at,
        scopes,
    };
    credential.validate()?;
    Ok(credential)
}
