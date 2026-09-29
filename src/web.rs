//! Native web search and page extraction.

mod extract;
mod http;
mod search;

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub action: String,
    pub url: Option<String>,
    pub query: Option<String>,
    pub provider: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub max_chars: Option<usize>,
}

#[derive(Clone)]
pub(crate) struct Settings {
    pub allow_private: bool,
    pub timeout: std::time::Duration,
    pub max_bytes: usize,
    pub searxng: Option<String>,
}

impl Settings {
    fn configured(value: Option<&Value>) -> Self {
        Self {
            allow_private: value
                .and_then(|v| v["allow_private"].as_bool())
                .unwrap_or(false),
            timeout: std::time::Duration::from_secs(
                value
                    .and_then(|v| v["timeout_secs"].as_u64())
                    .unwrap_or(20)
                    .clamp(1, 60),
            ),
            max_bytes: value
                .and_then(|v| v["max_bytes"].as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(2_000_000)
                .clamp(1024, 8_000_000),
            searxng: value.and_then(|v| v["searxng"].as_str()).map(str::to_owned),
        }
    }
}

pub fn run(arguments: Value, configuration: Option<&Value>) -> Result<Value, String> {
    if !crate::jail::Jail::from_env().allows_network() {
        return Err("web access requires the jail's reach grant".into());
    }
    let request: Request =
        serde_json::from_value(arguments).map_err(|why| format!("invalid web arguments: {why}"))?;
    let settings = Settings::configured(configuration);
    match request.action.as_str() {
        "search" => search::run(&request, &settings),
        "fetch" => {
            let url = request.url.as_deref().ok_or("fetch needs url")?;
            let fetched = http::fetch(url, &settings)?;
            let page = extract::page(&fetched.url, &fetched.body, &fetched.content_type);
            let chars: Vec<char> = page.text.chars().collect();
            let offset = request.offset.unwrap_or(0).min(chars.len());
            let end =
                (offset + request.max_chars.unwrap_or(12_000).clamp(1, 50_000)).min(chars.len());
            let text: String = chars[offset..end].iter().collect();
            Ok(json!({
                "url": fetched.url.as_str(), "status": fetched.status,
                "title": page.title, "content_type": fetched.content_type,
                "text": text, "links": page.links, "offset": offset,
                "total_chars": chars.len(), "next_offset": (end < chars.len()).then_some(end),
                "untrusted_content": true,
            }))
        }
        _ => Err("web action must be search or fetch".into()),
    }
}
