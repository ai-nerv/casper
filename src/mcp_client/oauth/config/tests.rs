use super::*;
use serde_json::{Value, json};

pub(super) fn configured() -> Config {
    serde_json::from_value(json!({"issuer":"https://issuer.example/tenant","resource":"https://resource.example/mcp","client_id":"native-client"})).expect("config")
}

#[test]
fn configuration_is_strict_bounded_and_binds_each_authority() {
    let config = configured();
    let binding = config.bind("https://resource.example/mcp").expect("bound");
    assert_eq!(binding.key().len(), 64);
    for field in ["issuer", "resource", "client_id"] {
        let mut changed = serde_json::to_value(&config).expect("value");
        changed[field] = match field {
            "issuer" => json!("https://issuer.example/other"),
            "resource" => json!("https://resource.example/other"),
            _ => json!("other-client"),
        };
        let changed: Config = serde_json::from_value(changed).expect("changed");
        assert_ne!(
            binding.key(),
            changed
                .bind("https://resource.example/mcp")
                .expect("bind")
                .key()
        );
    }
    assert_ne!(
        binding.key(),
        config
            .bind("https://resource.example/other")
            .expect("endpoint")
            .key()
    );
    for bad in [
        "http://resource.example/mcp",
        "https://user:secret@resource.example/mcp",
        "https://resource.example/mcp#fragment",
        "https://other.example/mcp",
    ] {
        assert_eq!(config.bind(bad), Err(Error::Configuration));
    }
    for (field, value) in [
        ("client_id", json!("")),
        ("client_id", json!("a".repeat(4097))),
        ("issuer", json!("https://issuer.example/?secret=canary")),
        ("resource", json!("https://resource.example/mcp\n")),
        ("timeout_secs", json!(0)),
        ("timeout_secs", json!(301)),
        ("scopes", json!(["read write"])),
        ("scopes", json!(["quoted\""])),
        ("scopes", json!(vec!["scope"; 33])),
        ("origins", json!(["https://endpoint.example/path"])),
    ] {
        let mut value_config = serde_json::to_value(&config).expect("value");
        value_config[field] = value;
        let bad: Config = serde_json::from_value(value_config).expect("shape");
        assert_eq!(
            bad.bind("https://resource.example/mcp"),
            Err(Error::Configuration),
            "{field}"
        );
    }
    let mut extra = serde_json::to_value(&config).expect("value");
    extra["client_secret"] = json!("never-in-errors");
    assert!(serde_json::from_value::<Config>(extra).is_err());
}

#[test]
fn metadata_cannot_broaden_endpoint_or_private_network_permission() {
    let mut config = configured();
    assert!(config.permits("https://issuer.example/token").is_ok());
    assert!(
        config
            .permits("https://resource.example/.well-known/oauth-protected-resource")
            .is_ok()
    );
    assert!(config.permits("https://other.example/token").is_err());
    config.origins.push("https://other.example".into());
    let other = config
        .permits("https://other.example/token")
        .expect("explicit origin");
    assert!(!config.private(&other));
    config.private_origins.push("https://other.example".into());
    assert!(config.private(&other));
    assert!(config.permits("https://unlisted.example/token").is_err());
    assert!(config.permits("http://127.0.0.1:3210/token").is_err());
    config.origins.push("http://127.0.0.1:3210".into());
    assert!(config.permits("http://127.0.0.1:3210/token").is_err());
    config.private_origins.push("http://127.0.0.1:3210".into());
    assert!(config.permits("http://127.0.0.1:3210/token").is_ok());
    assert!(config.permits("http://127.0.0.1:3211/token").is_err());
    assert!(config.permits("http://localhost:3210/token").is_err());
}

#[test]
fn native_server_auth_rejects_header_conflicts_and_stdio_without_echoing_material() {
    let auth = serde_json::to_value(configured()).expect("config");
    let good = json!({"remote":{"url":"https://resource.example/mcp","auth":auth}});
    assert!(crate::mcp_client::config::parse(Some(&good)).is_ok());
    for patch in [
        json!({"Authorization":"Bearer secret-canary"}),
        json!({"aUtHoRiZaTiOn":"secret-canary"}),
    ] {
        let mut bad = good.clone();
        bad["remote"]["headers"] = patch;
        let error = crate::mcp_client::config::parse(Some(&bad)).expect_err("conflict");
        assert!(!error.contains("secret-canary"));
    }
    let mut stdio = good.clone();
    stdio["remote"]["url"] = Value::Null;
    stdio["remote"]["command"] = json!("server");
    assert!(crate::mcp_client::config::parse(Some(&stdio)).is_err());
    let mut malformed = good;
    malformed["remote"]["auth"]["timeout_secs"] = json!("secret-canary");
    assert!(
        !crate::mcp_client::config::parse(Some(&malformed))
            .expect_err("type")
            .contains("secret-canary")
    );
}
