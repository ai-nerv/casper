use super::{Config, Discovery, Error, random};
use base64::Engine;
use sha2::{Digest, Sha256};
use url::Url;

pub(super) struct Proof {
    pub state: String,
    pub verifier: String,
}

impl Proof {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            state: random()?,
            verifier: random()?,
        })
    }

    pub fn challenge(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(self.verifier.as_bytes()))
    }

    pub fn authorization(
        &self,
        config: &Config,
        discovery: &Discovery,
        redirect: &str,
    ) -> Result<Url, Error> {
        let mut url = config.permits(&discovery.authorization_endpoint)?;
        let reserved = [
            "client_id",
            "response_type",
            "redirect_uri",
            "state",
            "scope",
            "resource",
            "code_challenge",
            "code_challenge_method",
        ];
        if url
            .query_pairs()
            .any(|(name, _)| reserved.contains(&name.as_ref()))
        {
            return Err(Error::Metadata);
        }
        url.query_pairs_mut()
            .append_pair("client_id", &config.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect)
            .append_pair("state", &self.state)
            .append_pair("resource", config.permits(&config.resource)?.as_str())
            .append_pair("code_challenge", &self.challenge())
            .append_pair("code_challenge_method", "S256");
        if !discovery.scopes.is_empty() {
            url.query_pairs_mut()
                .append_pair("scope", &discovery.scopes.join(" "));
        }
        if url.as_str().len() > 16384 {
            return Err(Error::Configuration);
        }
        Ok(url)
    }
}

pub(super) fn equal(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |different, (a, b)| different | (a ^ b))
        == 0
}
