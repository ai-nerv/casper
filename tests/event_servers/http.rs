use super::{check, fixture::Server};
use casper::{events::*, mcp_client};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub fn probe(mode: &str, receiver: &Receiver) {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("native crypto provider");
    use ServerOutcome::*;
    let (ready, address) = mpsc::sync_channel(1);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let discovery_failure = mode == "http-discovery";
    let thread = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("HTTP runtime")
            .block_on(async {
                let service = StreamableHttpService::new(
                    move || Ok(Server { discovery_failure }),
                    Arc::new(LocalSessionManager::default()),
                    StreamableHttpServerConfig::default(),
                );
                let app = axum::Router::new().nest_service("/private", service).layer(
                    axum::middleware::from_fn(
                        |request: axum::extract::Request, next: axum::middleware::Next| async move {
                            if request
                                .headers()
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                != Some("Bearer private-secret")
                            {
                                return axum::response::IntoResponse::into_response(
                                    axum::http::StatusCode::UNAUTHORIZED,
                                );
                            }
                            next.run(request).await
                        },
                    ),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("loopback listener");
                ready
                    .send(format!(
                        "http://{}/private",
                        listener.local_addr().expect("address")
                    ))
                    .expect("fixture ready");
                tokio::select! {
                    result = axum::serve(listener, app) => result.expect("serve"),
                    _ = stopped => {}
                }
            });
    });
    let url = address
        .recv_timeout(Duration::from_secs(5))
        .expect("HTTP readiness");
    let mut config = json!({"private":{"url":url,"timeout_secs":1,"headers":{"Authorization":if mode == "http-auth" {"Bearer private-wrong"} else {"Bearer private-secret"}}}});
    if mode == "http-oauth-missing" {
        let origin = url::Url::parse(&url)
            .expect("URL")
            .origin()
            .ascii_serialization();
        config["private"]
            .as_object_mut()
            .expect("server")
            .remove("headers");
        config["private"]["auth"] = json!({"issuer":origin,"resource":url,"client_id":"private-client","private_origins":[origin]});
    }
    let cards = mcp_client::cards(Some(&config)).expect("native catalog");
    if mode == "http" {
        assert_eq!(cards.len(), 1);
        let opened = check(receiver, &[Connecting, Connected]);
        assert_eq!(
            mcp_client::cards(Some(&config))
                .expect("reused catalog")
                .len(),
            1
        );
        let ran = mcp_client::call(
            Some(&config),
            "mcp__private__echo",
            &json!({"text":"private-argument"}),
        );
        assert!(!ran.failed, "{}", ran.said);
        let result: serde_json::Value = serde_json::from_str(&ran.said).expect("MCP reply");
        assert_eq!(result["structuredContent"]["echo"], "private-argument");
        check(receiver, &[]);
        mcp_client::shutdown();
        let closed = check(receiver, &[CloseRequested, Disconnected]);
        assert_eq!(opened[0].correlation, closed[0].correlation);
    } else {
        assert!(cards.is_empty());
        mcp_client::shutdown();
        check(
            receiver,
            &[
                Connecting,
                if mode == "http-oauth-missing" {
                    Refused
                } else {
                    Failed
                },
            ],
        );
    }
    stop.send(()).expect("stop HTTP fixture");
    thread.join().expect("HTTP fixture joined");
    assert_eq!(loss(), Loss::default());
}
