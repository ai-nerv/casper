use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn run(root: &std::path::Path, script: &str, refused: bool) {
    for reply in replies(root, script) {
        if refused {
            assert_eq!(reply["result"][0]["failed"], true, "{reply}");
            assert!(
                reply["result"][0]["said"]
                    .as_str()
                    .is_some_and(|text| text.contains("credential")),
                "{reply}"
            );
        } else {
            assert_ne!(reply["result"][0]["failed"], true, "{reply}");
        }
    }
}

/// The tools the socket server is started with, in place of the fixture's own `tools.lua`.
pub(super) const DECLARATION: &str = r#"
casper.tool("probe", {
    description = "Synthetic containment probe",
    parameters = { type = "object", properties = { script = { type = "string" } } },
    run = function(args)
        local done = casper.exec("/bin/sh", { "-c", args.script })
        return { said = done.out .. done.err, failed = done.code ~= 0 }
    end,
})
"#;

/// What a live `serve` answered to `script`, run twice on one connection.
pub(super) fn replies(root: &std::path::Path, script: &str) -> Vec<serde_json::Value> {
    let config = root.join("config/casper");
    std::fs::create_dir_all(&config).expect("socket config");
    std::fs::write(config.join("tools.lua"), DECLARATION).expect("socket declaration");
    let path = root.join("r/casper/s");
    {
        use std::os::unix::fs::DirBuilderExt;
        let runtime = root.join("r");
        let made = std::fs::DirBuilder::new().mode(0o700).create(&runtime);
        assert!(
            made.is_ok() || runtime.is_dir(),
            "private runtime directory"
        );
    }
    let key = "5e".repeat(32);
    let mut server = super::lifecycle::Owned::new(
        Command::new(env!("CARGO_BIN_EXE_casper"))
            .args(["serve", "--at"])
            .arg(&path)
            .env("XDG_RUNTIME_DIR", root.join("r"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("socket server"),
    );
    {
        use std::io::Write as _;
        let mut stdin = server.0.stdin.take().expect("serve's stdin");
        stdin
            .write_all(format!("{key}\n").as_bytes())
            .expect("the key");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&path) {
            break stream;
        }
        assert!(
            server.0.try_wait().expect("server status").is_none(),
            "socket server exited"
        );
        assert!(Instant::now() < deadline, "socket did not bind");
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("reply deadline");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("send deadline");
    (0..2)
        .map(|_| {
            let call = serde_json::json!({ "call": "run", "key": key, "args": [{
                "tool": "probe", "cwd": "/", "jail": "", "reach": true,
                "args": { "script": script, "CASPER_JAIL": "", "reach": true }
            }] });
            let call = serde_json::to_vec(&call).expect("call");
            casper::framing::write_frame(&mut stream, &call).expect("send call");
            let body = casper::framing::read_frame(&mut stream).expect("socket reply");
            let reply: serde_json::Value = serde_json::from_slice(&body).expect("reply JSON");
            assert_eq!(reply["ok"], true, "{reply}");
            reply
        })
        .collect()
}
