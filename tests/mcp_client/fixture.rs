use super::*;

pub(super) fn fixture() -> Scratch {
    let dir = Scratch::new("mc", "external");
    std::fs::create_dir_all(dir.join("casper")).expect("config");
    std::fs::create_dir_all(dir.join("work")).expect("workspace");
    std::fs::write(
        dir.join("casper/tools.lua"),
        include_str!("../../config/tools.lua"),
    )
    .expect("tools");
    std::fs::write(dir.join("proof.txt"), "External MCP reached a real tool.\n").expect("proof");
    std::fs::copy(dir.join("proof.txt"), dir.join("work/proof.txt")).expect("workspace proof");
    dir
}

pub(super) async fn invoke(
    dir: &Scratch,
    config: &Value,
    verb: &str,
    call: Option<Value>,
    jail: Option<&str>,
) -> Value {
    invoked(dir, config, verb, call, jail).await.0
}

/// [`invoke`], with what casper wrote on standard error.
pub(super) async fn invoked(
    dir: &Scratch,
    config: &Value,
    verb: &str,
    call: Option<Value>,
    jail: Option<&str>,
) -> (Value, String) {
    invoked_on(dir, config, verb, call, jail, true).await
}

pub(super) async fn invoked_on(
    dir: &Scratch,
    config: &Value,
    verb: &str,
    call: Option<Value>,
    jail: Option<&str>,
    bubblewrap: bool,
) -> (Value, String) {
    let namespace = NativeNamespace::new(dir, &dir.join("work"));
    let mut command =
        tokio::process::Command::from(namespace.command(env!("CARGO_BIN_EXE_casper")));
    command
        .arg(verb)
        .current_dir(dir.join("work"))
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
    if !bubblewrap {
        let paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .filter(|path| !path.join("bwrap").is_file())
            .collect();
        command.env(
            "PATH",
            std::env::join_paths(paths).expect("Landlock fixture PATH"),
        );
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

/// An external Casper exposing its native read tool inside containment.
pub(super) fn stdio_config() -> Value {
    json!({"mcp_servers":{"other":{"command":env!("CARGO_BIN_EXE_casper"),"args":["mcp","--tools=read"],"env":{"CASPER_CONFIGURE":"{}","CASPER_JAIL":"1"},"needs":"read"}}})
}
