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
    command_for(dir, "read,web")
}

fn command_for(dir: &Scratch, tools: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .args(["mcp", &format!("--tools={tools}")])
        .current_dir(dir)
        .env("HOME", &**dir)
        .env("XDG_CONFIG_HOME", &**dir)
        .env("XDG_DATA_HOME", &**dir)
        .env("XDG_RUNTIME_DIR", &**dir)
        .env("CASPER_CONFIGURE", "{}")
        .kill_on_drop(true)
        .env_remove("CASPER_JAIL");
    command
}

#[tokio::test]
async fn legacy_json_rpc_clients_negotiate_and_call_tools_without_private_wire_messages() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let dir = fixture();
    let mut child = command_for(&dir, "read")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("raw MCP server");
    let mut input = child.stdin.take().expect("MCP input");
    let mut output = BufReader::new(child.stdout.take().expect("MCP output")).lines();
    for (message, id) in [
        (
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"raw-test","version":"1"}}}),
            Some(1),
        ),
        (
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            Some(2),
        ),
        (
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read","arguments":{"path":"proof.txt"}}}),
            Some(3),
        ),
        (
            json!({"jsonrpc":"2.0","id":4,"method":"unknown/method","params":{}}),
            Some(4),
        ),
    ] {
        input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .expect("JSON-RPC input");
        if let Some(id) = id {
            let line = tokio::time::timeout(std::time::Duration::from_secs(5), output.next_line())
                .await
                .expect("MCP reply deadline")
                .expect("MCP output")
                .expect("MCP reply");
            let reply: serde_json::Value = serde_json::from_str(&line).expect("JSON-RPC reply");
            assert_eq!(reply["jsonrpc"], "2.0", "{reply}");
            assert_eq!(reply["id"], id, "{reply}");
            assert!(reply.get("ok").is_none(), "no family reply on MCP stdout");
            match id {
                1 => {
                    assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
                    assert_eq!(reply["result"]["serverInfo"]["name"], "casper");
                }
                2 => assert_eq!(reply["result"]["tools"][0]["name"], "read"),
                3 => assert!(
                    reply["result"]["content"][0]["text"]
                        .as_str()
                        .expect("tool text")
                        .contains("MCP really invoked")
                ),
                4 => assert_eq!(reply["error"]["code"], -32601),
                _ => unreachable!(),
            }
        }
    }
    input.shutdown().await.expect("input EOF");
    drop(input);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), child.wait())
            .await
            .expect("server shutdown deadline")
            .expect("server shutdown")
            .success()
    );
}

fn waiting_fixture() -> Scratch {
    let dir = fixture();
    std::fs::write(dir.join("casper/tools.lua"), r#"casper.tool('waiter', {
description='Wait until cancelled', parameters={type='object',properties={}},
run=function() local ran=casper.exec('sh',{'-c','echo $$ > waiting.pid; exec sleep 30'});return {said=ran.out,failed=ran.code~=0} end})"#).expect("waiting tool fixture");
    dir
}

async fn waiting_pid(dir: &Scratch) -> u32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    loop {
        if let Ok(pid) = std::fs::read_to_string(dir.join("waiting.pid")) {
            return pid.trim().parse().expect("waiting pid");
        }
        assert!(std::time::Instant::now() < deadline, "worker never started");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn assert_worker_stopped(pid: u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        if state.is_err() || state.is_ok_and(|state| state.split_whitespace().nth(2) == Some("Z")) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "MCP worker {pid} is still running"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn mcp_deadlines_stop_the_worker_and_preserve_the_server() {
    let dir = waiting_fixture();
    let mut command = command_for(&dir, "waiter");
    command.arg("--timeout-ms=2000");
    let client =
        ().serve(TokioChildProcess::new(command).expect("transport"))
            .await
            .expect("MCP initializes");
    let response = client
        .call_tool(CallToolRequestParams::new("waiter"))
        .await
        .expect("tool timeout result");
    assert_eq!(response.is_error, Some(true));
    assert!(
        response.content[0]
            .as_text()
            .expect("timeout message")
            .text
            .contains("timeout")
    );
    assert_worker_stopped(waiting_pid(&dir).await).await;
    assert_eq!(
        client
            .list_all_tools()
            .await
            .expect("server stays usable")
            .len(),
        1
    );
    client.cancel().await.expect("client closes");
}

#[tokio::test]
async fn mcp_request_cancellation_stops_the_worker_without_closing_the_connection() {
    let dir = waiting_fixture();
    let client =
        ().serve(TokioChildProcess::new(command_for(&dir, "waiter")).expect("transport"))
            .await
            .expect("MCP initializes");
    let request = serde_json::from_value(
        json!({"method":"tools/call","params":{"name":"waiter","arguments":{}}}),
    )
    .expect("call request");
    let handle = client
        .send_cancellable_request(request, Default::default())
        .await
        .expect("cancellable tool call");
    let pid = waiting_pid(&dir).await;
    handle
        .cancel(Some("test cancelled".into()))
        .await
        .expect("request cancelled");
    assert_worker_stopped(pid).await;
    assert_eq!(
        client
            .list_all_tools()
            .await
            .expect("connection remains usable")
            .len(),
        1
    );
    client.cancel().await.expect("client closes");
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
async fn disabled_and_hidden_tools_cannot_be_exposed_or_pollute_stdio() {
    let dir = fixture();
    for flag in ["off", "hidden"] {
        let mut command = command(&dir);
        let settings = json!({"tools":{"web":{flag:true}}});
        command.env("CASPER_CONFIGURE", settings.to_string());
        let output = command.output().await.expect("MCP starts");
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "stdout is exclusively MCP messages"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("no such configured tool: web"));
    }
}
