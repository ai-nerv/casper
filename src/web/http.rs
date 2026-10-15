use super::Settings;
use reqwest::blocking::Client;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use url::Url;

/// TLS for a fetch: Mozilla's roots as webpki-roots bundles them, hostnames checked, HTTP/1.1.
fn tls() -> Result<rustls::ClientConfig, String> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|why| format!("TLS could not start: {why}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

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
            .tls_backend_preconfigured(tls()?)
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
    use std::sync::Arc;

    #[test]
    fn a_certificate_no_bundled_root_signed_is_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![
                include_bytes!("../../tests/containment/tls/localhost.cert.der")
                    .to_vec()
                    .into(),
            ],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                include_bytes!("../../tests/containment/tls/localhost.key.der")
                    .to_vec()
                    .into(),
            ),
        )
        .expect("certificate");
        listener.set_nonblocking(true).expect("nonblocking");
        let served = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(why) => return Err(why),
                }
            };
            stream.set_nonblocking(false)?;
            let mut connection = rustls::ServerConnection::new(Arc::new(config)).expect("tls");
            while connection.is_handshaking() {
                connection.complete_io(&mut stream)?;
            }
            Ok::<_, std::io::Error>(())
        });
        let settings = super::Settings {
            allow_private: true,
            timeout: std::time::Duration::from_secs(10),
            max_bytes: 1024,
            searxng: None,
        };
        let fetched = super::fetch(&format!("https://localhost:{port}/"), &settings);
        assert!(fetched.is_err(), "an untrusted certificate was accepted");
        let refused = served
            .join()
            .expect("server")
            .expect_err("handshake completed");
        let alert = refused
            .get_ref()
            .and_then(|why| why.downcast_ref::<rustls::Error>());
        assert!(
            matches!(
                alert,
                Some(rustls::Error::AlertReceived(
                    rustls::AlertDescription::UnknownCA
                ))
            ),
            "{refused:?}"
        );
    }

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
