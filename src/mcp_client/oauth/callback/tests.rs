use super::*;
use crate::mcp_client::oauth::{Config, pkce::Proof, token};
use serde_json::json;

fn discovery() -> Discovery {
    Discovery {
        issuer: "https://issuer.example/tenant".into(),
        authorization_endpoint: "https://issuer.example/authorize".into(),
        token_endpoint: "https://issuer.example/token".into(),
        scopes: vec!["read".into()],
        require_response_issuer: true,
    }
}

fn request(callback: &Callback, query: &str) -> String {
    format!(
        "GET /oauth/callback?{query} HTTP/1.1\r\nHost: {}\r\n\r\n",
        callback.host
    )
}

#[test]
fn pkce_matches_rfc7636_and_rejects_metadata_parameter_injection() {
    let proof = Proof {
        state: "unpredictable-state".into(),
        verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
    };
    assert_eq!(
        proof.challenge(),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let fresh = Proof::new().expect("entropy");
    assert_eq!(fresh.state.len(), 64);
    assert_eq!(fresh.verifier.len(), 64);
    assert_ne!(fresh.state, fresh.verifier);
    let config: Config = serde_json::from_value(json!({"issuer":"https://issuer.example/tenant","resource":"https://resource.example/mcp","client_id":"native"})).expect("config");
    let mut metadata = discovery();
    let url = proof
        .authorization(&config, &metadata, "http://127.0.0.1:2345/oauth/callback")
        .expect("url");
    let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        pairs.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        pairs.get("resource").map(String::as_str),
        Some(config.resource.as_str())
    );
    assert!(!url.as_str().contains(&proof.verifier));
    for name in [
        "state",
        "client_id",
        "redirect_uri",
        "code_challenge",
        "resource",
    ] {
        metadata.authorization_endpoint =
            format!("https://issuer.example/authorize?{name}=injected");
        assert!(
            proof
                .authorization(&config, &metadata, "http://127.0.0.1:2345/oauth/callback")
                .is_err()
        );
    }
}

#[tokio::test]
async fn callback_requires_exact_state_issuer_host_and_single_parameters() {
    let callback = Callback::bind(0).await.expect("bind");
    let good = "state=expected&code=code-secret-canary&iss=https%3A%2F%2Fissuer.example%2Ftenant";
    assert_eq!(
        callback
            .parse(
                request(&callback, good).as_bytes(),
                "expected",
                &discovery()
            )
            .expect("code"),
        "code-secret-canary"
    );
    for query in [
        good.replace("state=expected", "state=wrong"),
        good.replace("state=expected&", ""),
        good.replace("code=code-secret-canary", "code="),
        good.replace("&iss=", "&issuer="),
        good.replace("%2Ftenant", "%2Ftenant%2F"),
        format!("{good}&state=expected"),
        format!("{good}&code=other"),
        format!("{good}&error=secret-canary"),
    ] {
        let error = callback
            .parse(
                request(&callback, &query).as_bytes(),
                "expected",
                &discovery(),
            )
            .expect_err("reject");
        assert!(!error.to_string().contains("secret-canary"));
    }
    let mut optional = discovery();
    optional.require_response_issuer = false;
    assert!(
        callback
            .parse(
                request(&callback, "state=expected&code=ok").as_bytes(),
                "expected",
                &optional
            )
            .is_ok()
    );
    assert!(
        callback
            .parse(
                request(
                    &callback,
                    "state=expected&code=ok&iss=https://wrong.example"
                )
                .as_bytes(),
                "expected",
                &optional
            )
            .is_err()
    );
    let wrong_host = request(&callback, good).replace(
        &format!("Host: {}", callback.host),
        "Host: attacker.example",
    );
    assert!(
        callback
            .parse(wrong_host.as_bytes(), "expected", &discovery())
            .is_err()
    );
}

#[tokio::test]
async fn native_callback_returns_no_secrets_and_releases_listener() {
    let callback = Callback::bind(0).await.expect("bind");
    let address = callback.listener.local_addr().expect("address");
    assert!(Callback::bind(address.port()).await.is_err());
    let request = request(
        &callback,
        "state=expected&code=code-secret-canary&iss=https%3A%2F%2Fissuer.example%2Ftenant",
    );
    let task = tokio::spawn(async move { callback.receive("expected", &discovery()).await });
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    socket.write_all(request.as_bytes()).await.expect("write");
    let mut reply = String::new();
    socket.read_to_string(&mut reply).await.expect("read");
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(!reply.contains("secret-canary"));
    assert_eq!(
        task.await.expect("task").expect("code"),
        "code-secret-canary"
    );
    assert!(Callback::bind(address.port()).await.is_ok());
    let callback = Callback::bind(0).await.expect("bind again");
    let address = callback.listener.local_addr().expect("address");
    let task = tokio::spawn(async move { callback.receive("state", &discovery()).await });
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    assert!(Callback::bind(address.port()).await.is_ok());
}

#[test]
fn token_merge_preserves_omitted_rotation_and_defines_null_and_expiry() {
    let previous = token::credential(br#"{"access_token":"old","token_type":"Bearer","refresh_token":"old-refresh","expires_in":100,"scope":"read"}"#, None, &[], 1000).expect("first");
    let merge = |value: serde_json::Value| {
        token::credential(
            &serde_json::to_vec(&value).expect("json"),
            Some(&previous),
            &[],
            1010,
        )
    };
    let omitted = merge(json!({"access_token":"new","token_type":"bearer","expires_in":200}))
        .expect("omitted");
    assert_eq!(omitted.refresh_token.as_deref(), Some("old-refresh"));
    assert_eq!(omitted.expires_at, Some(1210));
    assert_eq!(omitted.scopes, ["read"]);
    let consent = token::credential(
        br#"{"access_token":"new","token_type":"Bearer"}"#,
        Some(&previous),
        &["read".into(), "write".into()],
        1010,
    )
    .expect("expanded consent");
    assert_eq!(consent.scopes, ["read", "write"]);
    let rotated =
        merge(json!({"access_token":"new","token_type":"Bearer","refresh_token":"rotated"}))
            .expect("rotated");
    assert_eq!(rotated.refresh_token.as_deref(), Some("rotated"));
    assert_eq!(rotated.expires_at, None);
    let null = merge(json!({"access_token":"new","token_type":"Bearer","refresh_token":null}))
        .expect("explicit revoke");
    assert!(null.refresh_token.is_none());
    for value in [
        json!({"access_token":"secret-canary","token_type":"MAC"}),
        json!({"access_token":"secret-canary","token_type":"Bearer","error":"invalid_grant"}),
        json!({"access_token":"secret-canary","token_type":"Bearer","error":null}),
        json!({"access_token":"new","token_type":"Bearer","refresh_token":""}),
        json!({"access_token":"new","token_type":"Bearer","expires_in":0}),
        json!({"access_token":"new","token_type":"Bearer","expires_in":u64::MAX}),
    ] {
        assert_eq!(merge(value).expect_err("invalid"), Error::Credential);
    }
    assert!(
        token::credential(
            br#"{"access_token":"first","access_token":"second","token_type":"Bearer"}"#,
            None,
            &[],
            0
        )
        .is_err()
    );
}
