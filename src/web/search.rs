use super::{Request, Settings, extract, http};
use scraper::Html;
use serde_json::{Value, json};
use url::Url;

pub(crate) fn run(request: &Request, settings: &Settings) -> Result<Value, String> {
    let query = request
        .query
        .as_deref()
        .filter(|query| !query.trim().is_empty())
        .ok_or("search needs a nonempty query")?;
    if query.len() > 2000 {
        return Err("search query exceeds 2000 bytes".into());
    }
    let provider = request
        .provider
        .as_deref()
        .unwrap_or(if settings.searxng.is_some() {
            "searxng"
        } else {
            "duckduckgo"
        });
    let limit = request.limit.unwrap_or(5).clamp(1, 20);
    let results = match provider {
        "searxng" => {
            let endpoint = settings
                .searxng
                .as_deref()
                .ok_or("searxng requires trusted web.searxng configuration")?;
            let mut url =
                Url::parse(endpoint).map_err(|why| format!("invalid SearXNG endpoint: {why}"))?;
            url.query_pairs_mut()
                .append_pair("q", query)
                .append_pair("format", "json");
            let found = http::fetch(url.as_str(), settings)?;
            let value: Value = serde_json::from_str(&found.body)
                .map_err(|why| format!("SearXNG did not return JSON: {why}"))?;
            value["results"].as_array().ok_or("SearXNG response has no results")?.iter().filter_map(|result| {
                let url = Url::parse(result["url"].as_str()?).ok()?;
                if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() { return None; }
                Some(json!({"url": url.as_str(), "title": result["title"].as_str().unwrap_or_default(),
                    "snippet": result["content"].as_str().unwrap_or_default()}))
            }).take(limit).collect::<Vec<_>>()
        }
        "duckduckgo" => {
            let mut url =
                Url::parse("https://html.duckduckgo.com/html/").expect("fixed search URL");
            url.query_pairs_mut().append_pair("q", query);
            let found = http::fetch(url.as_str(), settings)?;
            let html = Html::parse_document(&found.body);
            if found.body.contains("anomaly.js") || found.body.contains("challenge-form") {
                return Err("DuckDuckGo returned a challenge; configure SearXNG instead of treating it as search results".into());
            }
            html.select(&extract::selector(".result")).filter_map(|result| {
                let link = result.select(&extract::selector("a.result__a")).next()?;
                let raw = found.url.join(link.value().attr("href")?).ok()?;
                let target = raw.query_pairs().find(|(key, _)| key == "uddg").map(|(_, target)| target.into_owned()).unwrap_or_else(|| raw.to_string());
                let url = Url::parse(&target).ok()?;
                if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() { return None; }
                let snippet = result.select(&extract::selector(".result__snippet")).next()
                    .map(|node| extract::clean(&node.text().collect::<String>())).unwrap_or_default();
                Some(json!({"url": url.as_str(), "title": extract::clean(&link.text().collect::<String>()), "snippet": snippet}))
            }).take(limit).collect::<Vec<_>>()
        }
        _ => return Err("search provider must be duckduckgo or searxng".into()),
    };
    Ok(json!({"provider": provider, "query": query, "results": results, "untrusted_content": true}))
}
