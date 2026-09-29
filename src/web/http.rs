use super::Settings;
use reqwest::blocking::Client;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use url::Url;

pub(crate) struct Fetched {
    pub url: Url,
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

pub(crate) fn public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_documentation()
                || octets[0] == 0
                || octets[0] >= 240
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 198 && (18..=19).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0))
        }
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => public(IpAddr::V4(ip)),
            None => {
                let segments = ip.segments();
                let special = (segments[0] == 0x2001
                    && (segments[1] < 0x0200 || segments[1] == 0x0db8))
                    || segments[0] == 0x2002
                    || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0);
                segments[0] & 0xe000 == 0x2000 && !special
            }
        },
    }
}

pub(crate) fn validated(
    source: &str,
    settings: &Settings,
) -> Result<(Url, Vec<SocketAddr>), String> {
    let url = Url::parse(source).map_err(|why| format!("invalid URL: {why}"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("only HTTP(S) URLs without embedded credentials are allowed".into());
    }
    let host = url
        .host_str()
        .ok_or("URL needs a host")?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url.port_or_known_default().ok_or("URL needs a port")?;
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = (host.as_str(), port)
            .to_socket_addrs()
            .map(|found| found.collect::<Vec<_>>());
        let _ = send.send(found);
    });
    let found = receive
        .recv_timeout(settings.timeout.min(std::time::Duration::from_secs(5)))
        .map_err(|_| "DNS resolution timed out".to_owned())?
        .map_err(|why| format!("DNS resolution failed: {why}"))?;
    if found.is_empty() {
        return Err("URL host resolved to no addresses".into());
    }
    if !settings.allow_private && found.iter().any(|address| !public(address.ip())) {
        return Err("private, loopback and reserved destinations require trusted web.allow_private configuration".into());
    }
    Ok((url, found))
}

pub(crate) fn fetch(source: &str, settings: &Settings) -> Result<Fetched, String> {
    let began = std::time::Instant::now();
    let mut destination = source.to_owned();
    for _ in 0..=5 {
        let (url, addresses) = validated(&destination, settings)?;
        let remaining = settings
            .timeout
            .checked_sub(began.elapsed())
            .ok_or("web request timed out")?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(remaining)
            .connect_timeout(remaining.min(std::time::Duration::from_secs(5)))
            .user_agent("Casper/0.5 web reader")
            .resolve_to_addrs(url.host_str().ok_or("URL has no host")?, &addresses)
            .build()
            .map_err(|why| format!("HTTP client could not start: {why}"))?;
        let response = client
            .get(url.clone())
            .send()
            .map_err(|why| format!("web request failed: {why}"))?;
        let status = response.status();
        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("redirect has no valid Location header")?;
            let next = url
                .join(location)
                .map_err(|why| format!("invalid redirect: {why}"))?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err("HTTPS downgrade redirect refused".into());
            }
            destination = next.to_string();
            continue;
        }
        if !status.is_success() {
            return Err(format!(
                "{} returned HTTP {}",
                url.origin().ascii_serialization(),
                status.as_u16()
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/plain")
            .to_owned();
        if !(content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml"))
        {
            return Err(format!("unsupported page content type: {content_type}"));
        }
        let mut body = Vec::new();
        response
            .take(settings.max_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|why| format!("page could not be read: {why}"))?;
        if body.len() > settings.max_bytes {
            return Err(format!(
                "page exceeds the {} byte download limit",
                settings.max_bytes
            ));
        }
        let body = String::from_utf8_lossy(&body).into_owned();
        return Ok(Fetched {
            url,
            status: status.as_u16(),
            content_type,
            body,
        });
    }
    Err("page exceeded five redirects".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_ordinary_public_destinations_are_allowed_without_private_permission() {
        for source in [
            "10.0.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "2001:2::1",
            "2002:a00:1::1",
            "3fff::1",
        ] {
            assert!(!super::public(source.parse().expect("address")), "{source}");
        }
        for source in [
            "8.8.8.8",
            "1.1.1.1",
            "::ffff:8.8.8.8",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
        ] {
            assert!(super::public(source.parse().expect("address")), "{source}");
        }
    }
}
