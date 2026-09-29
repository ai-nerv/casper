use casper::scratch::Scratch;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
use serde_json::json;

fn fixture() -> Scratch {
    let dir = Scratch::new("cm", "stdio");
    std::fs::create_dir_all(dir.join("casper")).expect("fixture config");
    std::fs::write(
        dir.join("casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("fixture tools");
    std::fs::write(
        dir.join("proof.txt"),
        "MCP really invoked Casper's read tool.\n",
    )
    .expect("fixture file");
    dir
}

fn command(dir: &Scratch) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .args(["mcp", "--tools=read,web"])
        .current_dir(dir)
        .env("HOME", &**dir)
        .env("XDG_CONFIG_HOME", &**dir)
        .env("XDG_DATA_HOME", &**dir)
        .env("XDG_RUNTIME_DIR", &**dir)
        .env("CASPER_CONFIGURE", "{}")
        .env_remove("CASPER_JAIL");
    command
}

#[tokio::test]
async fn an_official_mcp_client_discovers_schemas_and_calls_real_casper_tools() {
    let dir = fixture();
    let client =
        ().serve(TokioChildProcess::new(command(&dir)).expect("MCP child transport"))
            .await
            .expect("MCP initializes");
    let tools = client.list_all_tools().await.expect("MCP tool listing");
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().any(|tool| tool.name == "web"
        && tool.input_schema["properties"]["action"]["enum"] == json!(["search", "fetch"])));
    let read = client
        .call_tool(
            CallToolRequestParams::new("read").with_arguments(
                json!({"path":"proof.txt"})
                    .as_object()
                    .expect("arguments")
                    .clone(),
            ),
        )
        .await
        .expect("MCP tools/call");
    assert_eq!(read.is_error, Some(false));
    assert!(
        read.content[0]
            .as_text()
            .expect("MCP text")
            .text
            .contains("MCP really invoked")
    );
    let web = client
        .call_tool(
            CallToolRequestParams::new("web").with_arguments(
                json!({"action":"fetch", "url":"file:///etc/passwd"})
                    .as_object()
                    .expect("arguments")
                    .clone(),
            ),
        )
        .await
        .expect("MCP tool error is a result");
    assert_eq!(web.is_error, Some(true));
    assert!(
        web.content[0]
            .as_text()
            .expect("MCP error text")
            .text
            .contains("HTTP(S)")
    );
    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("write").with_arguments(
                    json!({"path":"proof.txt", "contents":"not permitted"})
                        .as_object()
                        .expect("arguments")
                        .clone()
                )
            )
            .await
            .is_err()
    );
    assert!(
        std::fs::read_to_string(dir.join("proof.txt"))
            .expect("fixture file")
            .contains("really invoked")
    );
    client.cancel().await.expect("MCP closes");
}

#[tokio::test]
async fn disabled_tools_cannot_be_exposed_and_startup_errors_do_not_pollute_stdio() {
    let dir = fixture();
    let mut command = command(&dir);
    command.env("CASPER_CONFIGURE", r#"{"tools":{"web":{"off":true}}}"#);
    let output = command.output().await.expect("MCP starts");
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is exclusively MCP messages"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("no such configured tool: web"));
}
