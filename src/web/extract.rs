use scraper::{Html, Selector};
use serde_json::{Value, json};
use url::Url;

pub(crate) struct Page {
    pub title: String,
    pub text: String,
    pub links: Vec<Value>,
}

pub(crate) fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static HTML selector")
}

pub(crate) fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn page(url: &Url, source: &str, content_type: &str) -> Page {
    if !content_type.contains("html") {
        return Page {
            title: String::new(),
            text: source.to_owned(),
            links: Vec::new(),
        };
    }
    let html = Html::parse_document(source);
    let title = html
        .select(&selector("title"))
        .next()
        .map(|node| clean(&node.text().collect::<String>()))
        .unwrap_or_default();
    let root = html
        .select(&selector("article, main, [role=main]"))
        .next()
        .or_else(|| html.select(&selector("body")).next())
        .unwrap_or_else(|| html.root_element());
    let mut text = String::new();
    for node in root.descendants() {
        if node.ancestors().any(|ancestor| {
            ancestor.value().as_element().is_some_and(|element| {
                matches!(
                    element.name(),
                    "script" | "style" | "nav" | "header" | "footer" | "template"
                )
            })
        }) {
            continue;
        }
        if let Some(element) = node.value().as_element()
            && matches!(
                element.name(),
                "p" | "div"
                    | "section"
                    | "article"
                    | "li"
                    | "br"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "pre"
                    | "tr"
            )
        {
            text.push('\n');
        }
        if let Some(value) = node.value().as_text() {
            text.push_str(value);
            text.push(' ');
        }
    }
    let text = text
        .lines()
        .map(clean)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let base = html
        .select(&selector("base[href]"))
        .next()
        .and_then(|node| node.value().attr("href"))
        .and_then(|base| url.join(base).ok())
        .unwrap_or_else(|| url.clone());
    let mut seen = std::collections::BTreeSet::new();
    let links = root
        .select(&selector("a[href]"))
        .filter_map(|node| {
            let url = base.join(node.value().attr("href")?).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || !seen.insert(url.to_string())
            {
                return None;
            }
            Some(json!({"url": url.as_str(), "text": clean(&node.text().collect::<String>())}))
        })
        .take(100)
        .collect();
    Page { title, text, links }
}
