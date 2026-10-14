use super::*;

/// A stdio server that answers `initialize` with a server name `size` bytes long, on one line,
/// echoing the request's id and protocol version, and then reads until it is closed.
fn long_winded(size: usize) -> Value {
    let script = r#"read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
version=$(printf '%s' "$request" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{},"serverInfo":{"name":"' "$id" "$version"
head -c "$1" /dev/zero | tr '\0' x
printf '","version":"1"}}}\n'
cat >/dev/null"#;
    json!({"command":"/bin/sh","args":["-c",script,"fixture",size.to_string()],"needs":"read"})
}

#[tokio::test]
async fn a_stdio_message_past_four_mebibytes_closes_the_session_and_says_so() {
    let dir = fixture();
    let manual = Some(json!({"tool":"tools","args":{"path":"mcp"}}));
    let small = json!({"mcp_servers":{"talker":long_winded(1024)}});
    let (reply, _) = invoked(&dir, &small, "run", manual.clone(), None).await;
    let said = reply["result"][0]["said"].as_str().expect("manual");
    assert!(said.contains("mcp/talker -- connected"), "{reply}");

    let large = json!({"mcp_servers":{"talker":long_winded(4 * 1024 * 1024 + 1)}});
    let (reply, stderr) = invoked(&dir, &large, "run", manual, None).await;
    let said = reply["result"][0]["said"].as_str().expect("manual");
    assert!(
        said.contains("mcp/talker -- connection/discovery failed"),
        "{reply}"
    );
    assert!(stderr.contains("over 4 MiB"), "{stderr}");
}

/// An HTTP server that answers `initialize` with a server name `size` bytes long, as one plain
/// JSON body framed by its length or by the connection's close, or as one SSE event.
async fn long_winded_http(
    size: usize,
    framing: &'static str,
) -> (Value, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP fixture");
    let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(answered(stream, size, framing));
        }
    });
    (json!({"url":url,"needs":"reach"}), task)
}

/// One request read off `stream`, and its answer written back before the connection closes.
async fn answered(stream: tokio::net::TcpStream, size: usize, framing: &str) {
    let mut reader = BufReader::new(stream);
    let (mut first, mut length) = (String::new(), 0);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
        if first.is_empty() {
            first = line;
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut stream = reader.into_inner();
    let head = if !first.starts_with("POST") {
        "HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            .to_owned()
    } else if request.get("id").is_none() {
        "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
    } else {
        let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{
            "protocolVersion":request["params"]["protocolVersion"],"capabilities":{},
            "serverInfo":{"name":"x".repeat(size),"version":"1"}}})
        .to_string();
        let (kind, reply) = match framing {
            "sse" => ("text/event-stream", format!("data: {reply}\n\n")),
            _ => ("application/json", reply),
        };
        let length = match framing {
            "length" => format!("content-length: {}\r\n", reply.len()),
            _ => String::new(),
        };
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\n{length}connection: close\r\n\r\n{reply}"
        )
    };
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[tokio::test]
async fn an_http_reply_past_four_mebibytes_closes_the_session_and_says_so() {
    let dir = fixture();
    let manual = Some(json!({"tool":"tools","args":{"path":"mcp"}}));
    for framing in ["length", "close", "sse"] {
        let (server, task) = long_winded_http(1024, framing).await;
        let config = json!({"mcp_servers":{"talker":server}});
        let (reply, stderr) = invoked(&dir, &config, "run", manual.clone(), None).await;
        let said = reply["result"][0]["said"].as_str().expect("manual");
        assert!(
            said.contains("mcp/talker -- connected"),
            "{framing}: {reply} {stderr}"
        );
        task.abort();

        let (server, task) = long_winded_http(4 * 1024 * 1024 + 1, framing).await;
        let config = json!({"mcp_servers":{"talker":server}});
        let (reply, stderr) = invoked(&dir, &config, "run", manual.clone(), None).await;
        let said = reply["result"][0]["said"].as_str().expect("manual");
        assert!(
            said.contains("mcp/talker -- connection/discovery failed"),
            "{framing}: {reply}"
        );
        assert!(stderr.contains("over 4 MiB"), "{framing}: {stderr}");
        task.abort();
    }
}

/// An HTTP server with one tool, `slow`, whose call is answered by an SSE stream holding a
/// notification with event id 1. With `past` zero the stream closes there, and the response
/// waits on the GET that resumes after id 1; otherwise the response follows in the stream, with
/// `past` bytes of text.
async fn resumable(past: usize) -> (Value, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP fixture");
    let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let call = Arc::new(std::sync::Mutex::new(Value::Null));
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(resumed(stream, Arc::clone(&call), past));
        }
    });
    (json!({"url":url,"needs":"reach","timeout_secs":10}), task)
}

/// One request read off `stream`, answered as [`resumable`] says.
async fn resumed(stream: tokio::net::TcpStream, call: Arc<std::sync::Mutex<Value>>, past: usize) {
    let mut reader = BufReader::new(stream);
    let (mut first, mut length, mut last) = (String::new(), 0, String::new());
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("last-event-id") {
                value.trim().clone_into(&mut last);
            }
        }
        if first.is_empty() {
            first = line;
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = &request["id"];
    let answer = |id: &Value, text: &str| json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}});
    let note = json!({"jsonrpc":"2.0","method":"notifications/message",
                      "params":{"level":"info","data":"halfway"}});
    let reply = match (first.split(' ').next(), request["method"].as_str()) {
        (Some("GET"), _) if last == "1" => {
            let asked = call.lock().expect("call").clone();
            let resumed = answer(&asked, "resumed after the notification");
            Some(("text/event-stream", format!("id: 2\ndata: {resumed}\n\n")))
        }
        (Some("POST"), _) if id.is_null() => None,
        (Some("POST"), Some("initialize")) => Some((
            "application/json",
            json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":request["params"]["protocolVersion"],
                "capabilities":{"tools":{}},"serverInfo":{"name":"resumable","version":"1"}}})
            .to_string(),
        )),
        (Some("POST"), Some("tools/list")) => Some((
            "application/json",
            json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                {"name":"slow","inputSchema":{"type":"object"}}]}})
            .to_string(),
        )),
        (Some("POST"), Some("tools/call")) => {
            id.clone_into(&mut call.lock().expect("call"));
            let rest = if past == 0 {
                String::new()
            } else {
                format!("data: {}\n\n", answer(id, &"x".repeat(past)))
            };
            Some((
                "text/event-stream",
                format!("retry: 10\nid: 1\ndata: {note}\n\n{rest}"),
            ))
        }
        _ => Some(("", String::new())),
    };
    let head = match reply {
        None => {
            "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
        }
        Some(("", _)) => {
            "HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_owned()
        }
        Some((kind, reply)) => {
            format!("HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\nconnection: close\r\n\r\n{reply}")
        }
    };
    let _ = reader.get_mut().write_all(head.as_bytes()).await;
    let _ = reader.get_mut().shutdown().await;
}

#[tokio::test]
async fn an_sse_reply_that_closes_before_its_response_resumes_after_its_last_event() {
    let dir = fixture();
    let (server, task) = resumable(0).await;
    let config = json!({"mcp_servers":{"talker":server}});
    let call = json!({"tool":"mcp__talker__slow","args":{}});
    let (reply, stderr) = invoked(&dir, &config, "run", Some(call), None).await;
    let ran = &reply["result"][0];
    assert_ne!(ran["failed"], true, "{reply} {stderr}");
    assert!(
        ran["said"]
            .as_str()
            .expect("said")
            .contains("resumed after the notification"),
        "{reply} {stderr}"
    );
    task.abort();
}

#[tokio::test]
async fn an_sse_event_past_four_mebibytes_fails_the_call_at_once_and_says_so() {
    let dir = fixture();
    let (server, task) = resumable(4 * 1024 * 1024 + 1).await;
    let config = json!({"mcp_servers":{"talker":server}});
    let call = json!({"tool":"mcp__talker__slow","args":{}});
    let began = std::time::Instant::now();
    let (reply, stderr) = invoked(&dir, &config, "run", Some(call), None).await;
    let ran = &reply["result"][0];
    assert_eq!(ran["failed"], true, "{reply} {stderr}");
    assert!(
        ran["said"].as_str().expect("said").contains("over 4 MiB"),
        "{reply} {stderr}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(8),
        "neither resumed nor retried until the deadline: {:?}",
        began.elapsed()
    );
    task.abort();
}

#[tokio::test]
async fn an_alias_that_would_catch_another_aliases_tools_is_refused() {
    let dir = fixture();
    for alias in ["other__read", "other_", "_other"] {
        let mut config = stdio_config();
        config["mcp_servers"][alias] = config["mcp_servers"]["other"].clone();
        let reply = invoke(&dir, &config, "tools", None, None).await;
        assert_eq!(reply["ok"], false, "{alias}: {reply}");
        assert!(
            reply["error"]
                .as_str()
                .expect("why")
                .contains("single underscores"),
            "{alias}: {reply}"
        );
    }
    let mut config = stdio_config();
    config["mcp_servers"]["other_two"] = json!({"command":"/no/such/server","timeout_secs":1});
    let reply = invoke(
        &dir,
        &config,
        "run",
        Some(json!({"tool":"mcp__other__read","args":{"path":"proof.txt"}})),
        None,
    )
    .await;
    assert!(
        reply["result"][0]["said"]
            .as_str()
            .expect("said")
            .contains("External MCP reached"),
        "{reply}"
    );
}

#[tokio::test]
async fn a_servers_loader_variables_reach_neither_the_launcher_nor_the_server() {
    for jail in [None, Some("1")] {
        let dir = fixture();
        let work = dir.join("work");
        std::fs::create_dir_all(work.join("casper")).expect("workspace config");
        std::fs::copy(dir.join("proof.txt"), work.join("proof.txt")).expect("workspace proof");
        std::fs::copy(dir.join("casper/tools.lua"), work.join("casper/tools.lua"))
            .expect("workspace declarations");
        let server = work.join("server");
        std::fs::copy(env!("CARGO_BIN_EXE_casper"), &server).expect("server executable");
        let observed = work.join("server-env");
        let config = json!({"mcp_servers":{"generic":{
            "command":"/bin/sh",
            "args":["-c","printf '%s|%s|%s' \"$MCP_FIXTURE_TOKEN\" \"$LD_PRELOAD\" \"$LD_LIBRARY_PATH\" > \"$1\"; exec \"$2\" mcp --tools=read","fixture",observed,server],
            "env":{"CASPER_CONFIGURE":"{}","CASPER_JAIL":"","XDG_CONFIG_HOME":work,
                   "MCP_FIXTURE_TOKEN":"explicit-fixture-token",
                   "LD_PRELOAD":"/nonexistent/casper-preload.so","LD_LIBRARY_PATH":"/nonexistent"},
            "needs":"read"
        }}});
        let (reply, stderr) = invoked(
            &dir,
            &config,
            "run",
            Some(json!({"tool":"mcp__generic__read","args":{"path":"proof.txt"}})),
            jail,
        )
        .await;
        assert!(
            !reply["result"][0]["failed"].as_bool().unwrap_or(false),
            "{jail:?}: {reply} {stderr}"
        );
        // Unjailed, the server inherits casper's own environment; nothing of the definition's.
        let seen = std::fs::read_to_string(&observed).expect("environment");
        assert!(
            seen.starts_with("explicit-fixture-token|"),
            "{jail:?}: {seen}"
        );
        assert!(!seen.contains("nonexistent"), "{jail:?}: {seen}");
        assert!(!stderr.contains("LD_PRELOAD"), "{jail:?}: {stderr}");
    }
}
