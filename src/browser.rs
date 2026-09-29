//! Native Chromium tools over a trusted loopback DevTools endpoint.

mod actions;
mod cdp;
mod managed;
mod supervisor;
pub use managed::Managed;
pub use managed::Owned;
pub use managed::{from_args, wait};

use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use url::Url;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    action: String,
    url: Option<String>,
    target: Option<String>,
    selector: Option<String>,
    value: Option<String>,
    key: Option<String>,
    x: Option<i32>,
    y: Option<i32>,
    max_chars: Option<usize>,
}

pub(super) struct Endpoint {
    url: Url,
    address: SocketAddr,
    timeout: Duration,
    allow_private: bool,
}

impl Endpoint {
    fn configured(configuration: Option<&Value>) -> Result<Self, String> {
        let supplied = std::env::var("CASPER_BROWSER_ENDPOINT").ok();
        let source = supplied.as_deref().or_else(|| configuration?.get("endpoint")?.as_str())
            .ok_or("browse needs a trusted browser endpoint; start `casper browser` and set CASPER_BROWSER_ENDPOINT")?;
        Self::parse(source, configuration)
    }

    fn parse(source: &str, configuration: Option<&Value>) -> Result<Self, String> {
        let url = Url::parse(source).map_err(|why| format!("invalid browser endpoint: {why}"))?;
        let ip: IpAddr = url
            .host_str()
            .ok_or("browser endpoint needs a host")?
            .trim_matches(['[', ']'])
            .parse()
            .map_err(|_| "browser endpoint must use a literal loopback IP")?;
        if url.scheme() != "http"
            || !ip.is_loopback()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("browser endpoint must be a credential-free loopback HTTP origin".into());
        }
        let port = url
            .port_or_known_default()
            .ok_or("browser endpoint needs a port")?;
        if port == 0 {
            return Err("browser endpoint port must not be zero".into());
        }
        Ok(Self {
            url,
            address: SocketAddr::new(ip, port),
            timeout: Duration::from_secs(
                configuration
                    .and_then(|v| v["timeout_secs"].as_u64())
                    .unwrap_or(20)
                    .clamp(1, 60),
            ),
            allow_private: configuration
                .and_then(|v| v["allow_private"].as_bool())
                .unwrap_or(false),
        })
    }

    fn request(&self, path: &str, create: bool) -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .build()
            .map_err(|why| why.to_string())?;
        let url = self.url.join(path).map_err(|why| why.to_string())?;
        let request = if create {
            client.put(url)
        } else {
            client.get(url)
        };
        let response = request
            .send()
            .map_err(|why| format!("browser endpoint unavailable: {why}"))?;
        if !response.status().is_success() {
            return Err(format!("browser endpoint returned {}", response.status()));
        }
        let mut body = Vec::new();
        response
            .take(1_000_001)
            .read_to_end(&mut body)
            .map_err(|why| why.to_string())?;
        if body.len() > 1_000_000 {
            return Err("browser discovery exceeded its response limit".into());
        }
        if path.starts_with("/json/close/") {
            return Ok(json!({"closed":true}));
        }
        serde_json::from_slice(&body).map_err(|why| format!("invalid browser discovery: {why}"))
    }

    fn target(&self, id: &str) -> Result<Value, String> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("invalid browser target identifier".into());
        }
        let tabs = self.request("/json/list", false)?;
        tabs.as_array()
            .ok_or("browser returned no tab list")?
            .iter()
            .find(|tab| tab["id"] == id && tab["type"] == "page")
            .cloned()
            .ok_or("no such browser page target".into())
    }
}

pub fn run(arguments: Value, configuration: Option<&Value>) -> Result<crate::tools::Ran, String> {
    if !crate::jail::Jail::from_env().allows_network() {
        return Err("browse requires the jail's reach grant".into());
    }
    let request: Request = serde_json::from_value(arguments)
        .map_err(|why| format!("invalid browse arguments: {why}"))?;
    let endpoint = Endpoint::configured(configuration)?;
    if request.action == "tabs" {
        let tabs = endpoint.request("/json/list", false)?;
        let pages = tabs
            .as_array()
            .ok_or("browser returned no tab list")?
            .iter()
            .filter(|tab| tab["type"] == "page")
            .take(100)
            .map(|tab| json!({"target":tab["id"],"title":tab["title"],"url":tab["url"]}))
            .collect::<Vec<_>>();
        return Ok(crate::tools::Ran::said(
            json!({"tabs":pages,"untrusted_content":true}).to_string(),
        ));
    }
    let navigation = if matches!(request.action.as_str(), "open" | "navigate") {
        Some(crate::web::validate_destination(
            request.url.as_deref().ok_or("navigation needs url")?,
            endpoint.allow_private,
        )?)
    } else {
        None
    };
    let page = if request.action == "open" {
        endpoint.request("/json/new?about:blank", true)?
    } else {
        endpoint.target(
            request
                .target
                .as_deref()
                .ok_or("browse action needs target; use open or tabs first")?,
        )?
    };
    let id = page["id"]
        .as_str()
        .ok_or("browser returned no target identifier")?;
    if request.action == "close" {
        return Ok(crate::tools::Ran::said(
            endpoint
                .request(&format!("/json/close/{id}"), false)?
                .to_string(),
        ));
    }
    let result = (|| {
        let mut cdp = cdp::Cdp::connect(
            &endpoint,
            page["webSocketDebuggerUrl"]
                .as_str()
                .ok_or("page has no debugger")?,
        )?;
        actions::run(&mut cdp, &request, navigation, id)
    })();
    if result.is_err() && request.action == "open" {
        let _ = endpoint.request(&format!("/json/close/{id}"), false);
    }
    result
}
