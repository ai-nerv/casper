use super::*;

#[tokio::test]
async fn own_mcp_directory_discovers_tools_and_routes_calls_without_coordinator_definitions() {
    let dir = fixture();
    let directory = dir.join("casper/mcp");
    std::fs::create_dir_all(&directory).expect("MCP directory");
    let server = stdio_config()["mcp_servers"]["other"].clone();
    std::fs::write(directory.join("generic.json"), server.to_string()).expect("definition");
    let reply = invoke(&dir, &json!({}), "tools", None, None).await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(
        reply["result"]
            .as_array()
            .expect("cards")
            .iter()
            .any(|card| card["name"] == "mcp__generic__read"),
        "{reply}"
    );
    let reply = invoke(
        &dir,
        &json!({}),
        "run",
        Some(json!({"tool":"mcp__generic__read","args":{"path":"proof.txt"}})),
        None,
    )
    .await;
    assert!(
        reply["result"][0]["said"]
            .as_str()
            .expect("result")
            .contains("External MCP reached a real tool"),
        "{reply}"
    );
    let reply = invoke(
        &dir,
        &json!({}),
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp"}})),
        None,
    )
    .await;
    assert!(
        reply["result"][0]["said"]
            .as_str()
            .expect("manual")
            .contains("mcp/generic -- connected"),
        "{reply}"
    );
}

#[tokio::test]
async fn other_application_registries_and_project_registries_are_never_imported() {
    let dir = fixture();
    for folder in [".codex", ".cursor", "Claude"] {
        std::fs::create_dir(dir.join(folder)).expect("foreign registry directory");
    }
    let json_registry =
        json!({"mcpServers":{"foreign":{"command":"/must/not/be/launched"}}}).to_string();
    for path in [
        ".cursor/mcp.json",
        ".claude.json",
        "Claude/claude_desktop_config.json",
        ".mcp.json",
        "mcp.json",
    ] {
        std::fs::write(dir.join(path), &json_registry).expect("foreign registry");
    }
    std::fs::write(
        dir.join(".codex/config.toml"),
        "[mcp_servers.foreign]\ncommand = '/must/not/be/launched'\n",
    )
    .expect("foreign TOML registry");
    let reply = invoke(&dir, &json!({}), "tools", None, None).await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(
        reply["result"]
            .as_array()
            .expect("cards")
            .iter()
            .all(|card| !card["name"].as_str().expect("name").starts_with("mcp_")),
        "{reply}"
    );
}

#[tokio::test]
async fn explicit_overrides_can_disable_directory_servers() {
    let dir = fixture();
    let directory = dir.join("casper/mcp");
    std::fs::create_dir_all(&directory).expect("MCP directory");
    std::fs::write(
        directory.join("generic.json"),
        r#"{"command":"/must/not/be/launched"}"#,
    )
    .expect("definition");
    let config = json!({"mcp_servers":{"generic":{"command":"disabled","off":true}}});
    let reply = invoke(&dir, &config, "tools", None, None).await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(
        reply["result"]
            .as_array()
            .expect("cards")
            .iter()
            .all(|card| !card["name"].as_str().expect("name").starts_with("mcp_"))
    );
}
