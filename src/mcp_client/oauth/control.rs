use super::{Config, Error};
use reqwest::{Method, StatusCode};
use std::{net::SocketAddr, time::Duration};

const LIMIT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

pub(in crate::mcp_client) async fn client(
    config: &Config,
    target: &str,
) -> Result<(url::Url, reqwest::Client), Error> {
    crate::web::crypto();
    tokio::time::timeout(TIMEOUT, async {
        if !crate::jail::Jail::from_env().allows_network() {
            return Err(Error::Network);
        }
        let url = config.permits(target)?;
        let host = url
            .host_str()
            .ok_or(Error::Configuration)?
            .trim_matches(['[', ']']);
        let port = url.port_or_known_default().ok_or(Error::Configuration)?;
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| Error::Network)?
            .take(65)
            .collect();
        if addresses.is_empty()
            || addresses.len() > 64
            || (!config.private(&url)
                && addresses
                    .iter()
                    .any(|address| !crate::web::public_address(address.ip())))
        {
            return Err(Error::Network);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(TIMEOUT)
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| Error::Network)?;
        Ok((url, client))
    })
    .await
    .map_err(|_| Error::Timeout)?
}

#[cfg(test)]
mod tests;

pub(super) struct Response {
    pub status: StatusCode,
    pub challenge: Option<String>,
    pub body: Vec<u8>,
}

pub(super) struct Control<'a> {
    config: &'a Config,
}

impl<'a> Control<'a> {
    pub fn new(config: &'a Config, endpoint: &str) -> Result<Self, Error> {
        config.bind(endpoint)?;
        crate::web::crypto();
        Ok(Self { config })
    }

    pub async fn request(
        &self,
        target: &str,
        form: Option<&[(String, String)]>,
        probe: bool,
    ) -> Result<Response, Error> {
        tokio::time::timeout(TIMEOUT, self.exchange(target, form, probe))
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn exchange(
        &self,
        target: &str,
        form: Option<&[(String, String)]>,
        probe: bool,
    ) -> Result<Response, Error> {
        let (url, client) = client(self.config, target).await?;
        let method = if form.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        let mut request = client
            .request(method, url)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(form) = form {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(form.iter().map(|(name, value)| (name, value)))
                .finish();
            if body.len() > LIMIT {
                return Err(Error::Credential);
            }
            request = request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body);
        }
        let mut response = request.send().await.map_err(|_| Error::Network)?;
        if response.status().is_redirection() {
            return Err(Error::Metadata);
        }
        let mut challenge = String::new();
        for header in response
            .headers()
            .get_all(reqwest::header::WWW_AUTHENTICATE)
        {
            let value = header.to_str().map_err(|_| Error::Metadata)?;
            if challenge
                .len()
                .saturating_add(value.len())
                .saturating_add(1)
                > 8192
            {
                return Err(Error::Metadata);
            }
            if !challenge.is_empty() {
                challenge.push(',');
            }
            challenge.push_str(value);
        }
        let status = response.status();
        let challenge = (!challenge.is_empty()).then_some(challenge);
        if probe {
            return Ok(Response {
                status,
                challenge,
                body: Vec::new(),
            });
        }
        if response
            .headers()
            .contains_key(reqwest::header::CONTENT_ENCODING)
            || response
                .content_length()
                .is_some_and(|length| length > LIMIT as u64)
        {
            return Err(Error::Metadata);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
            if body.len().saturating_add(chunk.len()) > LIMIT {
                return Err(Error::Metadata);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            challenge,
            body,
        })
    }
}
