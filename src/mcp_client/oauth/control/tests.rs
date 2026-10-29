use super::*;
use serde_json::json;
use std::{io::Read, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(origin: &str) -> Config {
    serde_json::from_value(json!({"issuer":origin,"resource":format!("{origin}/mcp"),"client_id":"native","private_origins":[origin]})).expect("config")
}

async fn request(socket: &mut tokio::net::TcpStream) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.expect("header"));
        assert!(header.len() <= 8192);
    }
    assert!(
        !String::from_utf8(header)
            .expect("header text")
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}

#[tokio::test]
async fn unannounced_oversize_and_encoded_control_bodies_are_refused() {
    for encoded in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let config = config(&origin);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            request(&mut socket).await;
            if encoded {
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await
                    .expect("headers");
            } else {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .await
                    .expect("headers");
                let body = format!("{:x}\r\n{}\r\n", LIMIT + 1, "x".repeat(LIMIT + 1));
                let _ = socket.write_all(body.as_bytes()).await;
            }
            let mut byte = [0];
            let closed = socket.read(&mut byte).await;
            assert!(
                matches!(closed, Ok(0)) || closed.is_err(),
                "peer must close"
            );
        });
        let target = format!("{origin}/token?secret-canary");
        let control = Control::new(&config, &config.resource).expect("control");
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                control.request(&target, None, false)
            )
            .await
            .expect("deadline"),
            Err(Error::Metadata)
        ));
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("closure deadline")
            .expect("server");
    }
}

#[tokio::test]
async fn dripping_body_cannot_extend_the_absolute_control_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let origin = format!("http://{}", listener.local_addr().expect("address"));
    let config = config(&origin);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64000\r\n\r\n").await.expect("headers");
        let (mut read, mut write) = socket.split();
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        let mut byte = [0];
        let mut sent = 0;
        loop {
            tokio::select! {
                result = read.read(&mut byte) => {
                    assert_eq!(result.expect("closure"), 0);
                    return sent;
                }
                _ = interval.tick() => {
                    if write.write_all(b" ").await.is_err() { return sent; }
                    sent += 1;
                }
            }
        }
    });
    let control = Control::new(&config, &config.resource).expect("control");
    let began = std::time::Instant::now();
    let result = tokio::time::timeout(
        TIMEOUT + Duration::from_secs(2),
        control.request(&format!("{origin}/token"), None, false),
    )
    .await
    .expect("outer deadline");
    assert!(matches!(result, Err(Error::Timeout)));
    assert!(began.elapsed() >= TIMEOUT);
    let sent = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("closure deadline")
        .expect("server");
    assert!(sent > 10 && sent < 64000);
}

#[tokio::test]
async fn invalid_tls_is_rejected_before_any_control_http_or_form_data() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let origin = format!(
        "https://localhost:{}",
        listener.local_addr().expect("address").port()
    );
    let config = config(&origin);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocols")
        .with_no_client_auth()
        .with_single_cert(
            vec![
                include_bytes!("../../../../tests/containment/tls/localhost.cert.der")
                    .to_vec()
                    .into(),
            ],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                include_bytes!("../../../../tests/containment/tls/localhost.key.der")
                    .to_vec()
                    .into(),
            ),
        )
        .expect("certificate");
    listener.set_nonblocking(true).expect("nonblocking");
    let server = std::thread::spawn(move || {
        let began = std::time::Instant::now();
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && began.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        socket.set_nonblocking(false).expect("blocking");
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("read timeout");
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("write timeout");
        let mut stream = rustls::StreamOwned::new(
            rustls::ServerConnection::new(Arc::new(tls)).expect("TLS"),
            socket,
        );
        let mut byte = [0];
        assert!(
            stream.read(&mut byte).is_err(),
            "untrusted TLS must not carry application bytes"
        );
    });
    let control = Control::new(&config, &config.resource).expect("control");
    let form = vec![("refresh_token".into(), "refresh-secret-canary".into())];
    let result = control
        .request(&format!("{origin}/token"), Some(&form), false)
        .await;
    assert!(matches!(result, Err(Error::Network)));
    server.join().expect("server");
}
