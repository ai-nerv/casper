use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn run(root: &std::path::Path, script: &str, refused: bool) {
    let config = root.join("config/casper");
    std::fs::create_dir_all(&config).expect("socket config");
    std::fs::write(
        config.join("tools.lua"),
        r#"
casper.tool("probe", {
    description = "Synthetic containment probe",
    parameters = { type = "object", properties = { script = { type = "string" } } },
    run = function(args)
        local done = casper.exec("/bin/sh", { "-c", args.script })
        return { said = done.out .. done.err, failed = done.code ~= 0 }
    end,
})
"#,
    )
    .expect("socket declaration");
    let path = root.join("r/casper/s");
    let mut server = super::lifecycle::Owned::new(
        Command::new(env!("CARGO_BIN_EXE_casper"))
            .args(["serve", "--at"])
            .arg(&path)
            .env("XDG_RUNTIME_DIR", root.join("r"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("socket server"),
    );
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
    for _ in 0..2 {
        let call = serde_json::json!({ "call": "run", "args": [{
            "tool": "probe", "cwd": "/", "jail": "", "reach": true,
            "args": { "script": script, "CASPER_JAIL": "", "reach": true }
        }] });
        casper::framing::write_frame(&mut stream, &serde_json::to_vec(&call).expect("call"))
            .expect("send call");
        let body = casper::framing::read_frame(&mut stream).expect("socket reply");
        let reply: serde_json::Value = serde_json::from_slice(&body).expect("reply JSON");
        assert_eq!(reply["ok"], true, "{reply}");
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
