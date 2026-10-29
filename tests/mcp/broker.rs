use super::*;
use serde_json::Value;
use tokio::io::AsyncWriteExt as _;

fn data(dir: &Scratch, sequence: u64, tool: &str) -> Value {
    json!({"version":1,"sequence":sequence,"root":dir.join("work"),"tool":tool,
        "args":{"path":"does-not-exist"},"data":{"kind":"text","data":{
            "source_text":"pushed data","text":"pushed data","source_bytes":11,
            "total_lines":1,"first_line":1,"last_line":1,"omitted_bytes":0}}})
}

fn params(tool: &str, envelope: Value) -> CallToolRequestParams {
    let mut params = CallToolRequestParams::new(tool.to_owned()).with_arguments(
        json!({"path":"does-not-exist"})
            .as_object()
            .expect("args")
            .clone(),
    );
    let mut meta = rmcp::model::RequestMetaObject::new();
    meta.insert(casper::mcp_client::read_broker::KEY.into(), envelope);
    params.meta = Some(meta);
    params
}

#[tokio::test]
async fn pushed_read_data_is_call_bound_single_use_and_never_acquires_a_registry() {
    let dir = fixture();
    let client =
        ().serve(TokioChildProcess::new(command_for(&dir, "read,shell")).expect("transport"))
            .await
            .expect("server");
    let tools = client.list_all_tools().await.expect("catalog");
    let read = tools
        .iter()
        .find(|tool| tool.name == "read")
        .expect("native read");
    assert!(
        read.meta
            .as_ref()
            .expect("extension")
            .contains_key(casper::mcp_client::read_broker::KEY)
    );
    let first = client
        .call_tool(params("read", data(&dir, 1, "read")))
        .await
        .expect("render");
    assert_ne!(first.is_error, Some(true), "{first:?}");
    assert!(
        serde_json::to_string(&first)
            .expect("result")
            .contains("pushed data")
    );
    for field in ["observations", "absence", "mutation", "revision_contract"] {
        assert!(
            first
                .structured_content
                .as_ref()
                .expect("structured result")
                .get(field)
                .is_none(),
            "{first:?}"
        );
    }
    let mut unknown = data(&dir, 2, "read");
    unknown["version"] = json!(2);
    let mut wrong_root = data(&dir, 3, "read");
    wrong_root["root"] = json!("/other");
    let mut wrong_path = data(&dir, 4, "read");
    wrong_path["args"]["path"] = json!("/protected");
    let mut forged = data(&dir, 5, "read");
    forged["data"]["data"]["revision"] = json!("forged");
    for envelope in [
        data(&dir, 1, "read"),
        unknown,
        wrong_root,
        wrong_path,
        data(&dir, 6, "shell"),
        forged,
    ] {
        let refused = client
            .call_tool(params("read", envelope))
            .await
            .expect("explicit refusal");
        assert_eq!(refused.is_error, Some(true), "{refused:?}");
    }
    let refused = client
        .call_tool(params("shell", data(&dir, 7, "shell")))
        .await
        .expect("nonread refusal");
    assert_eq!(refused.is_error, Some(true));
    client.cancel().await.expect("shutdown");
    assert_eq!(
        std::fs::read_dir(dir.join("var-tmp"))
            .expect("registry parent")
            .count(),
        0
    );
    assert!(!dir.join("work/does-not-exist").exists());
}

#[tokio::test]
async fn oversized_unterminated_stdio_requests_close_before_deserialization_or_acquisition() {
    let dir = fixture();
    let mut command = command_for(&dir, "read");
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("bounded server");
    let work = async {
        let mut stdin = child.stdin.take().expect("input pipe");
        let chunk = vec![b' '; 64 * 1024];
        let mut sent = 0;
        for _ in 0..128 {
            if stdin.write_all(&chunk).await.is_err() {
                break;
            }
            sent += chunk.len();
        }
        assert!(sent < 8 * 1024 * 1024, "unbounded input was accepted");
        drop(stdin);
        let output = child.wait_with_output().await.expect("server exit");
        assert!(!output.status.success(), "oversized startup was accepted");
        assert!(String::from_utf8_lossy(&output.stderr).contains("MCP startup failed"));
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .expect("bounded input deadline");
    assert_eq!(
        std::fs::read_dir(dir.join("var-tmp"))
            .expect("registry parent")
            .count(),
        0
    );
}
