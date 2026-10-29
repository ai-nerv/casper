use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    origin: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(mode: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (base, seen) = (origin.clone(), requests.clone());
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") && bytes.len() <= 8192 {
                    let byte = stream.read_u8().await.expect("header");
                    bytes.push(byte);
                }
                let header = String::from_utf8(bytes).expect("UTF8");
                assert!(!header.to_ascii_lowercase().contains("authorization:"));
                let path = header.split_whitespace().nth(1).expect("path").to_owned();
                seen.lock().expect("seen").push(path.clone());
                let (status, headers, body) = match path.as_str() {
                    "/mcp" if mode != "fallback" => (401, format!("WWW-Authenticate: Basic realm=\"other,realm\", Bearer resource_metadata=\"{base}/metadata\", scope=\"read\"\r\n"), String::new()),
                    "/mcp" => (405, String::new(), String::new()),
                    "/metadata" | "/.well-known/oauth-protected-resource" => {
                        let resource = if mode == "resource" { format!("{base}/other") } else { format!("{base}/mcp") };
                        (200, String::new(), json!({"resource":resource,"authorization_servers":[format!("{base}/tenant")],"scopes_supported":["read"]}).to_string())
                    },
                    "/.well-known/oauth-authorization-server/tenant" if mode != "fallback" => {
                        let issuer = if mode == "issuer" { format!("{base}/tenant/") } else { format!("{base}/tenant") };
                        let token = if mode == "origin" { "https://unlisted.invalid/token".into() } else { format!("{base}/token") };
                        let pkce = if mode == "pkce" { "plain" } else { "S256" };
                        (200, String::new(), json!({"issuer":issuer,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":token,"response_types_supported":["code"],"code_challenge_methods_supported":[pkce],"authorization_response_iss_parameter_supported":true}).to_string())
                    },
                    "/tenant/.well-known/openid-configuration" => (200, String::new(), json!({"issuer":format!("{base}/tenant"),"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"]}).to_string()),
                    _ => (404, String::new(), String::new()),
                };
                if path == "/metadata" && mode == "redirect" {
                    stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://unlisted.invalid/secret-canary\r\nContent-Length: 0\r\n\r\n").await.expect("redirect");
                } else if path == "/metadata" && mode == "huge" {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 65537\r\n\r\n")
                        .await
                        .expect("oversize");
                } else if path == "/metadata" && mode == "slow" {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                } else {
                    let reply = format!(
                        "HTTP/1.1 {status} Test\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(reply.as_bytes()).await.expect("reply");
                }
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }

    fn config(&self) -> Config {
        serde_json::from_value(json!({"issuer":format!("{}/tenant",self.origin),"resource":format!("{}/mcp",self.origin),"client_id":"registered-public-client","private_origins":[self.origin]})).expect("config")
    }
}

#[tokio::test]
async fn challenge_selects_resource_and_validates_exact_issuer_and_pkce() {
    let fixture = Fixture::new("valid").await;
    let config = fixture.config();
    let result = discover(&config, &format!("{}/mcp", fixture.origin))
        .await
        .expect("discovery");
    assert_eq!(result.issuer, config.issuer);
    assert_eq!(result.scopes, ["read"]);
    assert!(result.require_response_issuer);
    assert_eq!(
        *fixture.requests.lock().expect("requests"),
        [
            "/mcp",
            "/metadata",
            "/.well-known/oauth-authorization-server/tenant"
        ]
    );
}

#[tokio::test]
async fn fallback_order_covers_resource_and_both_oidc_layouts() {
    let fixture = Fixture::new("fallback").await;
    discover(&fixture.config(), &format!("{}/mcp", fixture.origin))
        .await
        .expect("fallback");
    assert_eq!(
        *fixture.requests.lock().expect("requests"),
        [
            "/mcp",
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-authorization-server/tenant",
            "/.well-known/openid-configuration/tenant",
            "/tenant/.well-known/openid-configuration"
        ]
    );
}

#[tokio::test]
async fn metadata_cannot_redirect_change_authorities_or_disable_pkce() {
    for mode in ["redirect", "resource", "issuer", "pkce", "origin", "huge"] {
        let fixture = Fixture::new(mode).await;
        let error = discover(&fixture.config(), &format!("{}/mcp", fixture.origin))
            .await
            .err()
            .expect("refused");
        assert!(
            matches!(error, Error::Metadata | Error::Configuration),
            "{mode}: {error}"
        );
        assert!(!error.to_string().contains("secret-canary"));
    }
}

#[tokio::test]
async fn absolute_timeout_covers_slow_headers_and_drops_request() {
    let fixture = Fixture::new("slow").await;
    let mut config = fixture.config();
    config.timeout_secs = 1;
    let began = std::time::Instant::now();
    assert!(matches!(
        discover(&config, &format!("{}/mcp", fixture.origin)).await,
        Err(Error::Timeout)
    ));
    assert!(began.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn challenges_are_bounded_and_reject_ambiguous_or_injected_values() {
    let fields = challenge::bearer("Basic realm=\"a,b\", Bearer scope=\"read write\", resource_metadata=\"https://example.test/meta\"").expect("challenge");
    assert_eq!(fields.get("scope").map(String::as_str), Some("read write"));
    for value in [
        "Bearer scope=\"read\",scope=\"write\"",
        "Bearer realm=\"x\", Bearer realm=\"y\"",
        "Bearer scope=\"unterminated",
        "Bearer scope=\"a\r\nb\"",
        "Bearer resource_metadata=\"bad\"trailing",
    ] {
        assert!(challenge::bearer(value).is_err(), "{value:?}");
    }
    assert!(challenge::bearer(&"a".repeat(8193)).is_err());
    assert_eq!(
        paths("https://issuer.test", false).expect("paths"),
        [
            "https://issuer.test/.well-known/oauth-authorization-server",
            "https://issuer.test/.well-known/openid-configuration"
        ]
    );
}
