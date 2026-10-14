#![allow(clippy::expect_used, clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use casper::scratch::Scratch;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[path = "mcp_client/directory.rs"]
mod directory;

#[path = "mcp_client/hardening.rs"]
mod hardening;

#[tokio::test]
async fn mcp_images_reach_the_caller_as_attachments_not_json_text() {
    let dir = fixture();
    let path = dir.join("raster.png");
    image::DynamicImage::new_rgba8(256, 128)
        .save_with_format(&path, image::ImageFormat::Png)
        .expect("raster");
    let reply = invoke(
        &dir,
        &stdio_config(),
        "run",
        Some(json!({"tool":"mcp__other__read","args":{"path":path}})),
        None,
    )
    .await;
    let ran: casper::tools::Ran = serde_json::from_value(reply["result"][0].clone()).expect("Ran");
    assert!(!ran.failed, "{}", ran.said);
    assert_eq!(ran.images.len(), 1);
    assert!(ran.said.contains("Image attachment"));
    assert!(!ran.said.contains(&ran.images[0].data));
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&ran.images[0].data)
        .expect("base64");
    assert_eq!(
        image::load_from_memory(&bytes).expect("raster").width(),
        256
    );
}

fn fixture() -> Scratch {
    let dir = Scratch::new("mc", "external");
    std::fs::create_dir_all(dir.join("casper")).expect("config");
    std::fs::write(
        dir.join("casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("tools");
    std::fs::write(dir.join("proof.txt"), "External MCP reached a real tool.\n").expect("proof");
    dir
}

async fn invoke(
    dir: &Scratch,
    config: &Value,
    verb: &str,
    call: Option<Value>,
    jail: Option<&str>,
) -> Value {
    invoked(dir, config, verb, call, jail).await.0
}

/// [`invoke`], with what casper wrote on standard error.
async fn invoked(
    dir: &Scratch,
    config: &Value,
    verb: &str,
    call: Option<Value>,
    jail: Option<&str>,
) -> (Value, String) {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .arg(verb)
        .current_dir(dir)
        .env("HOME", &**dir)
        .env("XDG_CONFIG_HOME", &**dir)
        .env("XDG_DATA_HOME", &**dir)
        .env("XDG_RUNTIME_DIR", &**dir)
        .env("CASPER_CONFIGURE", config.to_string())
        .env_remove("CASPER_JAIL")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(jail) = jail {
        std::fs::create_dir_all(dir.join("work")).expect("jailed workspace");
        command.current_dir(dir.join("work"));
        command.env("CASPER_JAIL", jail);
    }
    let mut child = command.spawn().expect("Casper");
    let mut input = child.stdin.take().expect("input");
    if let Some(call) = call {
        input
            .write_all(call.to_string().as_bytes())
            .await
            .expect("call");
    }
    drop(input);
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .expect("deadline")
        .expect("output");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reply = serde_json::from_slice(&out.stdout).unwrap_or_else(|why| {
        panic!(
            "{why}: {} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (reply, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Another casper as the external server, with its own jail explicitly off: `casper mcp` with none
/// named runs each tool inside the floor, and these are tests of the client.
fn stdio_config() -> Value {
    json!({"mcp_servers":{"other":{"command":env!("CARGO_BIN_EXE_casper"),"args":["mcp","--tools=read"],"env":{"CASPER_CONFIGURE":"{}","CASPER_JAIL":""},"needs":"read"}}})
}

async fn socket_call(path: &std::path::Path, body: Value) -> Value {
    let mut socket = tokio::net::UnixStream::connect(path)
        .await
        .expect("connect");
    let body = body.to_string();
    socket
        .write_all(&(body.len() as u32).to_be_bytes())
        .await
        .expect("length");
    socket.write_all(body.as_bytes()).await.expect("frame");
    let mut header = [0; 4];
    socket.read_exact(&mut header).await.expect("header");
    let mut reply = vec![0; u32::from_be_bytes(header) as usize];
    socket.read_exact(&mut reply).await.expect("reply");
    serde_json::from_slice(&reply).expect("JSON")
}

#[tokio::test]
async fn persistent_casper_reuses_external_sessions_and_stops_owned_children() {
    let dir = fixture();
    let pid_file = dir.join("server-pids");
    // A runtime directory of its own: serve binds only beneath private ones, and makes `casper/`.
    let run = dir.join("run");
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(&run)
        .expect("runtime");
    let socket = run.join("casper/pooled.sock");
    let config = json!({"mcp_servers":{"other":{
        "command":"/bin/sh", "args":["-c","echo $$ >> \"$1\"; exec \"$2\" mcp --tools=read","fixture",pid_file,env!("CARGO_BIN_EXE_casper")],
        "env":{"CASPER_CONFIGURE":"{}","CASPER_JAIL":""}, "needs":"read"
    }}});
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_casper"))
        .args(["serve", "--at"])
        .arg(&socket)
        .current_dir(&*dir)
        .env("HOME", &*dir)
        .env("XDG_CONFIG_HOME", &*dir)
        .env("XDG_RUNTIME_DIR", &run)
        .env("CASPER_CONFIGURE", config.to_string())
        .env_remove("CASPER_JAIL")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("persistent Casper");
    // `serve` answers only calls carrying the key it read first on its standard input.
    let key = "5e".repeat(32);
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(format!("{key}\n").as_bytes())
        .await
        .expect("the serve key");
    drop(stdin);
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .await
        .expect("ready");
    assert!(ready.contains("listening"), "{ready}");
    let cards = socket_call(&socket, json!({"call":"tools","args":[],"key":key})).await;
    assert_eq!(cards["ok"], true, "{cards}");
    for _ in 0..2 {
        let ran = socket_call(
            &socket,
            json!({"call":"run","args":[{"tool":"mcp__other__read","args":{"path":"proof.txt"}}],"key":key}),
        )
        .await;
        assert!(
            ran["result"][0]["said"]
                .as_str()
                .expect("said")
                .contains("External MCP reached"),
            "{ran}"
        );
    }
    let pids = std::fs::read_to_string(pid_file).expect("server PID");
    assert_eq!(
        pids.lines().count(),
        1,
        "server restarted between calls: {pids}"
    );
    let pid: u32 = pids.trim().parse().expect("PID");
    child.kill().await.expect("stop Casper");
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => break,
                Ok(stat)
                    if stat
                        .split_once(") ")
                        .is_some_and(|(_, fields)| fields.starts_with('Z')) =>
                {
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("owned external server stopped with Casper");
}

#[tokio::test]
async fn stdio_discovers_real_schemas_and_calls_a_generic_server() {
    let dir = fixture();
    let config = stdio_config();
    let reply = invoke(&dir, &config, "tools", None, None).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let tool = reply["result"]
        .as_array()
        .expect("cards")
        .iter()
        .find(|tool| tool["name"] == "mcp__other__read")
        .expect("imported tool");
    assert_eq!(tool["parameters"]["properties"]["path"]["type"], "string");
    let ran = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__other__read","args":{"path":"proof.txt"}})),
        None,
    )
    .await;
    assert_eq!(ran["ok"], true, "{ran}");
    assert!(
        ran["result"][0]["said"]
            .as_str()
            .expect("result")
            .contains("External MCP reached a real tool.")
    );
    let result: Value = serde_json::from_str(ran["result"][0]["said"].as_str().expect("said"))
        .expect("original MCP result");
    assert!(result.get("structuredContent").is_some(), "{result}");
}

#[tokio::test]
async fn mcp_manual_reports_support_even_without_configured_servers() {
    let dir = fixture();
    let groups = invoke(
        &dir,
        &json!({}),
        "run",
        Some(json!({"tool":"tools","args":{}})),
        None,
    )
    .await;
    assert!(
        groups["result"][0]["said"]
            .as_str()
            .expect("groups")
            .contains("mcp --")
    );
    let reply = invoke(
        &dir,
        &json!({}),
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp"}})),
        None,
    )
    .await;
    let said = reply["result"][0]["said"].as_str().expect("manual");
    assert!(
        !reply["result"][0]["failed"].as_bool().unwrap_or(false),
        "{reply}"
    );
    assert!(said.contains("MCP support is available"));
    assert!(said.contains("No MCP servers are configured"));
}

#[tokio::test]
async fn mcp_manual_exposes_live_tools_and_schemas_without_credentials() {
    let dir = fixture();
    let mut config = stdio_config();
    config["mcp_servers"]["other"]["env"]["SECRET"] = json!("fixture-secret");
    config["mcp_servers"]["disabled"] = json!({"command":"private-command","off":true});
    config["tools"] = json!({"mcp__other__shell":{"hidden":true}});
    config["mcp_servers"]["other"]["excluded_tools"] = json!(["write"]);
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp"}})),
        None,
    )
    .await;
    let said = reply["result"][0]["said"].as_str().expect("manual");
    assert!(said.contains("mcp/other -- connected"), "{said}");
    assert!(said.contains("mcp/disabled -- disabled"));
    assert!(said.contains("mcp__other__read"));
    assert!(!said.contains("mcp__other__write"));
    assert!(!said.contains("mcp__other__shell"));
    assert!(!said.contains("fixture-secret"));
    assert!(!said.contains("private-command"));
    let page = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp/other/mcp__other__read"}})),
        None,
    )
    .await;
    assert!(
        page["result"][0]["said"]
            .as_str()
            .expect("schema")
            .contains("Arguments:")
    );
    let denied = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__other__write","args":{}})),
        None,
    )
    .await;
    assert_eq!(denied["result"][0]["failed"], true);
}

#[tokio::test]
async fn unavailable_mcp_servers_do_not_remove_native_or_healthy_tools() {
    let dir = fixture();
    let mut config = stdio_config();
    config["mcp_servers"]["broken"] = json!({"command":"/no/such/server","timeout_secs":1});
    let reply = invoke(&dir, &config, "tools", None, None).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let cards = reply["result"].as_array().expect("cards");
    assert!(cards.iter().any(|card| card["name"] == "read"));
    assert!(cards.iter().any(|card| card["name"] == "mcp__other__read"));
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp"}})),
        None,
    )
    .await;
    let said = reply["result"][0]["said"].as_str().expect("manual");
    assert!(
        said.contains("mcp/broken -- connection/discovery failed"),
        "{said}"
    );
    assert!(said.contains("mcp/other -- connected"), "{said}");
}

#[tokio::test]
async fn startup_catalog_can_be_read_without_launching_servers_or_network_grants() {
    let dir = fixture();
    let config = json!({"mcp_servers":{"generic":{"command":"/must/not/be/launched","network":true}},"mcp_catalog":[
        {"name":"mcp__generic__lookup","description":"Lookup a record","parameters":{"type":"object"},"group":"mcp:generic"},
        {"name":"mcp__generic__hidden","description":"private tool","parameters":{},"group":"mcp:generic"}
    ],"tools":{"mcp__generic__hidden":{"hidden":true}}});
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp"}})),
        Some("1"),
    )
    .await;
    let said = reply["result"][0]["said"].as_str().expect("manual");
    assert!(
        said.contains("1 tools discovered at session startup"),
        "{reply}"
    );
    assert!(said.contains("mcp__generic__lookup"));
    assert!(!said.contains("mcp__generic__hidden"));
    assert!(!said.contains("connection/discovery failed"));
    let hidden = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"tools","args":{"path":"mcp/generic/mcp__generic__hidden"}})),
        Some("1"),
    )
    .await;
    assert_eq!(hidden["result"][0]["failed"], true);
}

#[tokio::test]
async fn trusted_stdio_environment_survives_the_process_jail() {
    let dir = fixture();
    let work = dir.join("work");
    std::fs::create_dir_all(work.join("casper")).expect("workspace config");
    std::fs::copy(dir.join("proof.txt"), work.join("proof.txt")).expect("workspace proof");
    std::fs::copy(dir.join("casper/tools.lua"), work.join("casper/tools.lua"))
        .expect("workspace declarations");
    let server = work.join("server");
    std::fs::copy(env!("CARGO_BIN_EXE_casper"), &server).expect("readable server executable");
    let observed = work.join("server-env");
    let config = json!({"mcp_servers":{"generic":{
        "command":"/bin/sh", "args":["-c","printf '%s' \"$MCP_FIXTURE_TOKEN\" > \"$1\"; exec \"$2\" mcp --tools=read","fixture",observed,server],
        "env":{"CASPER_CONFIGURE":"{}","CASPER_JAIL":"","XDG_CONFIG_HOME":work,"MCP_FIXTURE_TOKEN":"explicit-fixture-token"},"needs":"read"
    }}});
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__generic__read","args":{"path":"proof.txt"}})),
        Some("1"),
    )
    .await;
    assert!(
        !reply["result"][0]["failed"].as_bool().unwrap_or(false),
        "{reply}"
    );
    assert_eq!(
        std::fs::read_to_string(observed).expect("environment"),
        "explicit-fixture-token"
    );
}

#[tokio::test]
async fn configured_allowlists_and_disabled_tools_are_enforced_on_calls() {
    let dir = fixture();
    let mut config = stdio_config();
    config["mcp_servers"]["other"]["tools"] = json!([]);
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__other__read","args":{"path":"proof.txt"}})),
        None,
    )
    .await;
    assert_eq!(reply["result"][0]["failed"], true);
    assert!(
        reply["result"][0]["said"]
            .as_str()
            .expect("why")
            .contains("no such configured")
    );
    config["mcp_servers"]["other"]["tools"] = json!(["read"]);
    config["tools"] = json!({"mcp__other__read":{"off":true}});
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__other__read","args":{"path":"proof.txt"}})),
        None,
    )
    .await;
    assert_eq!(reply["ok"], false);
}

#[tokio::test]
async fn invalid_configuration_is_reported_not_silently_ignored() {
    let dir = fixture();
    for settings in [
        json!({"command":"true","url":"http://127.0.0.1/mcp"}),
        json!({"url":"file:///tmp/anything"}),
        json!({"command":"true","typo":true}),
        json!({"command":"true","timeout_secs":0}),
    ] {
        let reply = invoke(
            &dir,
            &json!({"mcp_servers":{"bad":settings}}),
            "tools",
            None,
            None,
        )
        .await;
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"].as_str().expect("error").contains("MCP")
                || reply["error"]
                    .as_str()
                    .expect("error")
                    .contains("mcp_servers")
        );
    }
}

#[derive(Clone)]
struct HttpServer {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl ServerHandler for HttpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools: Vec<Tool> = serde_json::from_value(json!([
            {"name":"echo","description":"A generic echo, not Blender","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}},
            {"name":"fail","description":"Return a tool error","inputSchema":{"type":"object"}},
            {"name":"wait","description":"Wait for cancellation","inputSchema":{"type":"object"}}
        ])).expect("schemas");
        let rest = tools.split_off(1);
        let second = request.and_then(|one| one.cursor).is_some();
        Ok(ListToolsResult {
            tools: if second { rest } else { tools },
            next_cursor: if second { None } else { Some("next".into()) },
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match params.name.as_ref() {
            "echo" => {
                let fields = params.arguments.unwrap_or_default();
                let mut result = CallToolResult::success(vec![ContentBlock::text(
                    fields.get("text").and_then(Value::as_str).unwrap_or(""),
                )]);
                result.structured_content = Some(json!({"proof":"generic-http"}));
                Ok(result.into())
            }
            "fail" => Ok(CallToolResult::error(vec![ContentBlock::text(
                "deliberate remote failure",
            )])
            .into()),
            "wait" => {
                context.ct.cancelled().await;
                self.cancelled
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(CallToolResult::error(vec![ContentBlock::text("cancelled")]).into())
            }
            _ => Err(ErrorData::invalid_params("unknown tool", None)),
        }
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(
            serde_json::from_value(json!({"resources":[{"uri":"test://proof","name":"Proof"}]}))
                .expect("resources"),
        )
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        assert_eq!(params.uri, "test://proof");
        Ok(serde_json::from_value::<ReadResourceResult>(json!({"contents":[{"uri":"test://proof","mimeType":"text/plain","text":"resource proof"}]})).expect("resource").into())
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(
            serde_json::from_value(
                json!({"prompts":[{"name":"example","description":"A prompt"}]}),
            )
            .expect("prompts"),
        )
    }

    async fn get_prompt(
        &self,
        _: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        Ok(serde_json::from_value::<GetPromptResult>(
            json!({"messages":[{"role":"user","content":{"type":"text","text":"prompt proof"}}]}),
        )
        .expect("prompt")
        .into())
    }
}

async fn http_fixture() -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let state = cancelled.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(HttpServer {
                cancelled: state.clone(),
            })
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| async move {
                if request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    != Some("Bearer fixture-secret")
                {
                    return axum::response::IntoResponse::into_response(
                        axum::http::StatusCode::UNAUTHORIZED,
                    );
                }
                next.run(request).await
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP fixture");
    let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (url, task, cancelled)
}

#[tokio::test]
async fn streamable_http_discovers_calls_preserves_errors_resources_and_prompts() {
    let dir = fixture();
    let (url, task, _) = http_fixture().await;
    let config = json!({"mcp_servers":{"remote":{"url":url,"needs":"reach","headers":{"Authorization":"Bearer fixture-secret"}}}});
    let tools = invoke(&dir, &config, "tools", None, None).await;
    assert_eq!(tools["ok"], true, "{tools}");
    for name in [
        "mcp__remote__echo",
        "mcp_remote_resources",
        "mcp_remote_prompts",
    ] {
        assert!(
            tools["result"]
                .as_array()
                .expect("cards")
                .iter()
                .any(|card| card["name"] == name)
        );
    }
    for (name, args, expected, failed) in [
        (
            "mcp__remote__echo",
            json!({"text":"HTTP proof"}),
            "generic-http",
            false,
        ),
        (
            "mcp__remote__fail",
            json!({}),
            "deliberate remote failure",
            true,
        ),
        (
            "mcp_remote_resources",
            json!({"action":"list"}),
            "test://proof",
            false,
        ),
        (
            "mcp_remote_resources",
            json!({"action":"read","uri":"test://proof"}),
            "resource proof",
            false,
        ),
        (
            "mcp_remote_prompts",
            json!({"action":"list"}),
            "example",
            false,
        ),
        (
            "mcp_remote_prompts",
            json!({"action":"get","name":"example"}),
            "prompt proof",
            false,
        ),
    ] {
        let reply = invoke(
            &dir,
            &config,
            "run",
            Some(json!({"tool":name,"args":args})),
            None,
        )
        .await;
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(
            reply["result"][0]["failed"].as_bool().unwrap_or(false),
            failed,
            "{reply}"
        );
        assert!(
            reply["result"][0]["said"]
                .as_str()
                .expect("said")
                .contains(expected),
            "{reply}"
        );
    }
    task.abort();
}

#[tokio::test]
async fn network_denials_and_call_deadlines_are_enforced() {
    let dir = fixture();
    let (url, task, cancelled) = http_fixture().await;
    let config = json!({"mcp_servers":{"remote":{"url":url,"timeout_secs":1,"headers":{"Authorization":"Bearer fixture-secret"}}}});
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__remote__echo","args":{"text":"denied"}})),
        Some("1"),
    )
    .await;
    assert_eq!(reply["result"][0]["failed"], true, "{reply}");
    assert!(
        reply["result"][0]["said"]
            .as_str()
            .expect("why")
            .contains("network grant")
    );
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__remote__wait","args":{}})),
        None,
    )
    .await;
    assert_eq!(reply["result"][0]["failed"], true, "{reply}");
    assert!(
        reply["result"][0]["said"]
            .as_str()
            .expect("why")
            .contains("not retried")
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("remote cancellation");
    task.abort();
}
