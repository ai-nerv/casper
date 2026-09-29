use super::{Request, cdp::Cdp};
use crate::tools::{Image, Ran};
use base64::Engine;
use serde_json::{Value, json};

const SNAPSHOT: &str = r#"(()=>{
const visible = e => !!(e.offsetWidth || e.offsetHeight || e.getClientRects().length);
const cut = (s,n=200) => Array.from(s||'').slice(0,n).join('');
return {url:location.href,title:cut(document.title,500),text:cut(document.body?.innerText,LIMIT),
 elements:Array.from(document.querySelectorAll('a,button,input,textarea,select,[role="button"]')).filter(visible).slice(0,100).map(e=>({
 tag:e.tagName.toLowerCase(),id:e.id||undefined,role:e.getAttribute('role')||undefined,
 name:cut(e.getAttribute('aria-label')||e.getAttribute('placeholder')||e.innerText),
 href:e.tagName==='A'?e.href:undefined,type:e.type||undefined,
 value:e.type==='password'?undefined:cut(e.value)})),untrusted_content:true};})()"#;

pub(super) fn run(
    cdp: &mut Cdp,
    request: &Request,
    navigation: Option<String>,
    id: &str,
) -> Result<Ran, String> {
    let value = match request.action.as_str() {
        "open" | "navigate" => {
            cdp.call("Page.enable", json!({}))?;
            cdp.call("Page.setDownloadBehavior", json!({"behavior":"deny"}))?;
            let reply = cdp.call("Page.navigate", json!({"url":navigation.ok_or("navigation needs url")?}))?;
            if let Some(error) = reply.get("errorText") { return Err(format!("navigation failed: {error}")); }
            cdp.navigated(reply["loaderId"].as_str())?;
            snapshot(cdp, request)?
        }
        "snapshot" => snapshot(cdp, request)?,
        "click" => {
            let selector = selector(request)?;
            cdp.evaluate(format!("(()=>{{const e=document.querySelector({selector});if(!e)throw Error('selector not found');e.scrollIntoView({{block:'center'}});e.click();return true;}})()"))?;
            cdp.ready()?;
            snapshot(cdp, request)?
        }
        "type" => {
            let selector = selector(request)?;
            let value = request.value.as_deref().ok_or("type needs value")?;
            if value.len() > 10_000 { return Err("typed value exceeds 10000 bytes".into()); }
            let value = serde_json::to_string(value).map_err(|why| why.to_string())?;
            cdp.evaluate(format!("(()=>{{const e=document.querySelector({selector});if(!e||!(e instanceof HTMLInputElement||e instanceof HTMLTextAreaElement))throw Error('selector must identify an input or textarea');e.focus();const p=e instanceof HTMLInputElement?HTMLInputElement.prototype:HTMLTextAreaElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(e,{value});e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));return true;}})()"))?;
            snapshot(cdp, request)?
        }
        "press" => {
            let key = request.key.as_deref().ok_or("press needs key")?;
            let code = match key {
                "Enter" => 13, "Tab" => 9, "Escape" => 27, "Backspace" => 8,
                "Delete" => 46, "ArrowLeft" => 37, "ArrowUp" => 38, "ArrowRight" => 39,
                "ArrowDown" => 40, "Home" => 36, "End" => 35, "PageUp" => 33,
                "PageDown" => 34, "Space" => 32,
                _ => return Err("unsupported key; use Enter, Tab, Escape, Backspace, Delete, arrows or page keys".into()),
            };
            let key = if key == "Space" { " " } else { key };
            for kind in ["keyDown", "keyUp"] {
                cdp.call("Input.dispatchKeyEvent", json!({"type":kind,"key":key,"windowsVirtualKeyCode":code}))?;
            }
            cdp.ready()?;
            snapshot(cdp, request)?
        }
        "scroll" => {
            let x = request.x.unwrap_or(0).clamp(-10_000, 10_000);
            let y = request.y.unwrap_or(600).clamp(-10_000, 10_000);
            cdp.evaluate(format!("(()=>{{window.scrollBy({x},{y});return {{x:scrollX,y:scrollY}};}})()"))?
        }
        "screenshot" => {
            let reply = cdp.call("Page.captureScreenshot", json!({"format":"png","captureBeyondViewport":false}))?;
            let data = reply["data"].as_str().ok_or("browser returned no screenshot")?;
            if data.len() > 1_500_000 { return Err("browser screenshot exceeds the 1.5 MB image limit".into()); }
            let decoded = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| "invalid screenshot encoding")?;
            if !decoded.starts_with(b"\x89PNG\r\n\x1a\n") { return Err("browser screenshot is not PNG".into()); }
            return Ok(Ran {
                said: json!({"target":id,"format":"png","untrusted_content":true}).to_string(),
                images: vec![Image { data: data.into(), mime_type: "image/png".into() }],
                ..Ran::default()
            });
        }
        _ => return Err("browse action must be tabs, open, navigate, snapshot, click, type, press, scroll, screenshot or close".into()),
    };
    let mut value = value;
    if let Some(value) = value.as_object_mut() {
        value.insert("target".into(), json!(id));
    }
    Ok(Ran::said(value.to_string()))
}

fn selector(request: &Request) -> Result<String, String> {
    let selector = request
        .selector
        .as_deref()
        .ok_or("DOM action needs selector")?;
    if selector.is_empty() || selector.len() > 2000 {
        return Err("selector must contain 1..2000 bytes".into());
    }
    serde_json::to_string(selector).map_err(|why| why.to_string())
}

fn snapshot(cdp: &mut Cdp, request: &Request) -> Result<Value, String> {
    cdp.evaluate(
        SNAPSHOT.replace(
            "LIMIT",
            &request
                .max_chars
                .unwrap_or(12_000)
                .clamp(1, 50_000)
                .to_string(),
        ),
    )
}
