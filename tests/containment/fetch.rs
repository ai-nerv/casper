use std::io::{Read, Write};
use std::net::{TcpListener, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A synthetic certificate for `localhost`, made for this test and trusted by nothing.
const CERT: &[u8] = include_bytes!("tls/localhost.cert.der");
const KEY: &[u8] = include_bytes!("tls/localhost.key.der");

/// What the listener answers, and what the jailed fetch must read back.
const BODY: &str = "CASPER_HTTPS_FETCHED";

/// An HTTPS listener on the loopback for one request, the port it is on, and the probe that
/// fetches from it inside the jail. The thread answers whether a whole request was served.
pub(super) fn serve(dir: &Path) -> std::thread::JoinHandle<bool> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fetch listener");
    let port = listener.local_addr().expect("fetch address").port();
    std::fs::write(dir.join("work/fetch-port"), port.to_string()).expect("fetch port");
    std::fs::copy(
        std::env::current_exe().expect("test executable"),
        dir.join("work/fetch-probe"),
    )
    .expect("fetch probe executable");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CERT.to_vec().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(KEY.to_vec().into()),
        )
        .expect("server certificate");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    std::thread::spawn(move || answer(&listener, Arc::new(config)))
}

/// Serve one request within the deadline, over TLS, and close.
fn answer(listener: &TcpListener, config: Arc<rustls::ServerConfig>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return false,
        }
    };
    let timeout = Some(Duration::from_secs(10));
    if stream.set_nonblocking(false).is_err() || stream.set_read_timeout(timeout).is_err() {
        return false;
    }
    let Ok(connection) = rustls::ServerConnection::new(config) else {
        return false;
    };
    let mut tls = rustls::StreamOwned::new(connection, stream);
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while !request.windows(4).any(|end| end == b"\r\n\r\n") {
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
    }
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    );
    let sent = tls.write_all(reply.as_bytes()).and_then(|()| tls.flush());
    tls.conn.send_close_notify();
    let _ = tls.flush();
    sent.is_ok() && request.starts_with(b"GET / ")
}

#[test]
fn probe() {
    if std::env::current_exe().expect("executable").file_name()
        != Some(std::ffi::OsStr::new("fetch-probe"))
    {
        return;
    }
    let port: u16 = std::fs::read_to_string("fetch-port")
        .expect("fetch port")
        .trim()
        .parse()
        .expect("port number");
    let resolved: Vec<_> = ("localhost", port)
        .to_socket_addrs()
        .expect("localhost resolves inside the jail")
        .collect();
    assert!(
        resolved.iter().any(|at| at.ip().is_loopback()),
        "{resolved:?}"
    );
    casper::web::crypto();
    let client = reqwest::blocking::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(15))
        .build()
        .expect("HTTPS client");
    let body = client
        .get(format!("https://localhost:{port}/"))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .expect("an HTTPS fetch from inside the jail");
    assert_eq!(body, BODY);
}
