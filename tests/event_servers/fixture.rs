use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
};
use serde_json::json;

pub(super) struct Server {
    pub(super) discovery_failure: bool,
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.discovery_failure {
            return Err(ErrorData::invalid_params("private-discovery-error", None));
        }
        Ok(serde_json::from_value(json!({"tools":[{"name":"echo","description":"private-description","inputSchema":{"type":"object"}}]})).expect("schema"))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        assert_eq!(params.name.as_ref(), "echo");
        let arguments = params.arguments.expect("literal arguments");
        if arguments.get("text").and_then(serde_json::Value::as_str) == Some("private-hold") {
            use std::io::Write;
            let mut executions = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(".private-executions")
                .expect("execution count");
            writeln!(executions, "hold").expect("record actual execution");
            std::fs::write(".private-entered", std::process::id().to_string())
                .expect("entered native request");
            context.ct.cancelled().await;
            return Err(ErrorData::invalid_params("private-cancelled", None));
        }
        let mut result = CallToolResult::success(vec![ContentBlock::text("private-output")]);
        result.structured_content =
            Some(json!({"pid":std::process::id(), "echo": arguments.get("text")}));
        Ok(result.into())
    }
}

pub fn serve(mode: &str) {
    if mode == "timeout" {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("server runtime")
        .block_on(async {
            Server {
                discovery_failure: mode == "discovery",
            }
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await
            .expect("native initialize")
            .waiting()
            .await
            .expect("native shutdown");
        });
}

pub fn host_pid(namespace_pid: u32) -> u32 {
    let expected = std::env::current_dir()
        .expect("workspace")
        .join("private-server");
    host_pid_at(namespace_pid, &expected)
}

pub fn host_pid_at(namespace_pid: u32, expected: &std::path::Path) -> u32 {
    let matching: Vec<u32> = std::fs::read_dir("/proc")
        .expect("process table")
        .filter_map(Result::ok)
        .filter(|entry| {
            std::fs::read_link(entry.path().join("exe")).is_ok_and(|path| path == expected)
        })
        .filter_map(|entry| {
            let status =
                std::fs::read_to_string(entry.path().join("status")).expect("owned server status");
            let inner = status
                .lines()
                .find(|line| line.starts_with("NSpid:"))
                .and_then(|line| line.split_whitespace().last())
                .expect("namespace PID");
            assert_eq!(inner.parse::<u32>().expect("PID"), namespace_pid);
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
        })
        .collect();
    assert_eq!(matching.len(), 1, "one selected native server executable");
    *matching.first().expect("host server PID")
}
