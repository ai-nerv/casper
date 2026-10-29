use super::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub issuer: String,
    pub resource: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default)]
    pub private_origins: Vec<String>,
    #[serde(default)]
    pub callback_port: u16,
    #[serde(default = "patience")]
    pub timeout_secs: u64,
}

fn patience() -> u64 {
    120
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    endpoint: String,
    resource: String,
    issuer: String,
    client_id: String,
}

fn uri(value: &str) -> Result<Url, Error> {
    if value.len() > 4096 || value.bytes().any(|byte| byte <= 0x20 || byte == 0x7f) {
        return Err(Error::Configuration);
    }
    let parsed = Url::parse(value).map_err(|_| Error::Configuration)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Error::Configuration);
    }
    Ok(parsed)
}

fn origin(value: &str) -> Result<String, Error> {
    let url = uri(value)?;
    if url.path() != "/" || url.query().is_some() {
        return Err(Error::Configuration);
    }
    Ok(url.origin().ascii_serialization())
}

pub(super) fn scope(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}

impl Config {
    pub fn bind(&self, endpoint: &str) -> Result<Binding, Error> {
        if !(1..=300).contains(&self.timeout_secs)
            || self.scopes.len() > 32
            || !self.scopes.iter().all(|s| scope(s))
            || self.origins.len() > 16
            || self.private_origins.len() > 16
            || self.client_id.is_empty()
            || self.client_id.len() > 4096
            || self.client_id.bytes().any(|b| b <= 0x20 || b == 0x7f)
        {
            return Err(Error::Configuration);
        }
        for item in self.origins.iter().chain(&self.private_origins) {
            origin(item)?;
        }
        let endpoint = uri(endpoint)?;
        let resource = uri(&self.resource)?;
        let issuer = uri(&self.issuer)?;
        if issuer.query().is_some() || endpoint.origin() != resource.origin() {
            return Err(Error::Configuration);
        }
        for item in [&endpoint, &resource, &issuer] {
            self.secure(item)?;
        }
        if self.client_id.starts_with("http:") || self.client_id.starts_with("https:") {
            let client = uri(&self.client_id)?;
            if client.scheme() != "https" || client.query().is_some() {
                return Err(Error::Configuration);
            }
        }
        Ok(Binding {
            endpoint: endpoint.into(),
            resource: resource.into(),
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
        })
    }

    fn secure(&self, url: &Url) -> Result<(), Error> {
        let loopback = url
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback());
        if url.scheme() != "https" && !(loopback && self.private(url)) {
            return Err(Error::Configuration);
        }
        Ok(())
    }

    pub fn permits(&self, value: &str) -> Result<Url, Error> {
        let url = uri(value)?;
        self.secure(&url)?;
        let wanted = url.origin();
        let allowed = uri(&self.issuer)?.origin() == wanted
            || uri(&self.resource)?.origin() == wanted
            || self
                .origins
                .iter()
                .any(|item| origin(item).is_ok_and(|item| item == wanted.ascii_serialization()));
        if !allowed {
            return Err(Error::Configuration);
        }
        Ok(url)
    }

    pub fn private(&self, url: &Url) -> bool {
        self.private_origins
            .iter()
            .any(|item| origin(item).is_ok_and(|item| item == url.origin().ascii_serialization()))
    }
}

impl Binding {
    pub(super) fn validate(&self) -> Result<(), Error> {
        let endpoint = uri(&self.endpoint)?;
        let resource = uri(&self.resource)?;
        let issuer = uri(&self.issuer)?;
        if endpoint.as_str() != self.endpoint
            || resource.as_str() != self.resource
            || endpoint.origin() != resource.origin()
            || issuer.query().is_some()
            || self.client_id.is_empty()
            || self.client_id.len() > 4096
            || self
                .client_id
                .bytes()
                .any(|byte| byte <= 0x20 || byte == 0x7f)
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }

    pub fn key(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"casper-mcp-oauth-store-v1");
        for field in [
            &self.endpoint,
            &self.resource,
            &self.issuer,
            &self.client_id,
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn resource(&self) -> &str {
        &self.resource
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

#[cfg(test)]
mod tests;
