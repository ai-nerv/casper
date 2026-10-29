use super::{Config, Error, challenge, config, control::Control};
use serde::Deserialize;
use url::Url;

#[derive(Deserialize)]
struct Resource {
    resource: String,
    authorization_servers: Vec<String>,
    scopes_supported: Option<Vec<String>>,
    bearer_methods_supported: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Server {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    code_challenge_methods_supported: Vec<String>,
    response_types_supported: Vec<String>,
    grant_types_supported: Option<Vec<String>>,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    authorization_response_iss_parameter_supported: bool,
    #[serde(default)]
    client_id_metadata_document_supported: bool,
}

pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub scopes: Vec<String>,
    pub require_response_issuer: bool,
}

fn paths(source: &str, resource: bool) -> Result<Vec<String>, Error> {
    let url = Url::parse(source).map_err(|_| Error::Metadata)?;
    let base = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let suffix = if resource {
        "oauth-protected-resource"
    } else {
        "oauth-authorization-server"
    };
    let mut result = vec![format!("{base}/.well-known/{suffix}{path}")];
    if resource {
        if !path.is_empty() {
            result.push(format!("{base}/.well-known/{suffix}"));
        }
    } else {
        result.push(format!("{base}/.well-known/openid-configuration{path}"));
        if !path.is_empty() {
            result.push(format!("{base}{path}/.well-known/openid-configuration"));
        }
    }
    Ok(result)
}

async fn document<T: serde::de::DeserializeOwned>(
    control: &Control<'_>,
    paths: Vec<String>,
) -> Result<T, Error> {
    for path in paths {
        let response = control.request(&path, None, false).await?;
        if matches!(response.status.as_u16(), 404 | 405) {
            continue;
        }
        if !response.status.is_success() {
            return Err(Error::Metadata);
        }
        return serde_json::from_slice(&response.body).map_err(|_| Error::Metadata);
    }
    Err(Error::Metadata)
}

/// Discover the configured resource's selected issuer without credentials or interactive consent.
pub async fn discover(config: &Config, endpoint: &str) -> Result<Discovery, Error> {
    let control = Control::new(config, endpoint)?;
    tokio::time::timeout(
        std::time::Duration::from_secs(config.timeout_secs.min(60)),
        async {
            let probe = control.request(endpoint, None, true).await?;
            let challenge = if probe.status == reqwest::StatusCode::UNAUTHORIZED {
                challenge::bearer(probe.challenge.as_deref().unwrap_or("Bearer"))?
            } else {
                Default::default()
            };
            let locations = match challenge.get("resource_metadata") {
                Some(location) => vec![location.clone()],
                None => paths(endpoint, true)?,
            };
            let resource: Resource = document(&control, locations).await?;
            if config.permits(&resource.resource)?.as_str() != config.bind(endpoint)?.resource()
                || resource.authorization_servers.is_empty()
                || resource.authorization_servers.len() > 16
                || !resource
                    .authorization_servers
                    .iter()
                    .any(|issuer| issuer == &config.issuer)
                || resource
                    .bearer_methods_supported
                    .as_ref()
                    .is_some_and(|methods| !methods.iter().any(|method| method == "header"))
            {
                return Err(Error::Metadata);
            }
            let server: Server = document(&control, paths(&config.issuer, false)?).await?;
            if server.issuer != config.issuer
                || !server
                    .code_challenge_methods_supported
                    .iter()
                    .any(|method| method == "S256")
                || !server
                    .response_types_supported
                    .iter()
                    .any(|kind| kind == "code")
                || server
                    .grant_types_supported
                    .as_ref()
                    .is_some_and(|grants| !grants.iter().any(|grant| grant == "authorization_code"))
                || server
                    .token_endpoint_auth_methods_supported
                    .as_ref()
                    .is_some_and(|methods| !methods.iter().any(|method| method == "none"))
                || (config.client_id.starts_with("https:")
                    && !server.client_id_metadata_document_supported)
            {
                return Err(Error::Metadata);
            }
            config.permits(&server.authorization_endpoint)?;
            config.permits(&server.token_endpoint)?;
            let requested = challenge
                .get("scope")
                .map(|value| value.split(' ').map(str::to_owned).collect())
                .unwrap_or_else(|| resource.scopes_supported.unwrap_or_default());
            let scopes = if config.scopes.is_empty() {
                requested
            } else {
                if requested.iter().any(|scope| !config.scopes.contains(scope)) {
                    return Err(Error::Metadata);
                }
                config.scopes.clone()
            };
            if scopes.len() > 32 || !scopes.iter().all(|scope| config::scope(scope)) {
                return Err(Error::Metadata);
            }
            Ok(Discovery {
                issuer: server.issuer,
                authorization_endpoint: server.authorization_endpoint,
                token_endpoint: server.token_endpoint,
                scopes,
                require_response_issuer: server.authorization_response_iss_parameter_supported,
            })
        },
    )
    .await
    .map_err(|_| Error::Timeout)?
}

#[cfg(test)]
mod tests;
