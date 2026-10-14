//! The hexe and oslo client libraries casper lends a declaration: the family's frame cap on both
//! directions, and no path built from `$UID`.

use casper::lua::engine::Engine;
use casper::scratch::Scratch;
use std::process::{Command, Stdio};

/// A transport whose every connection answers `reply` and counts how often it was dialled.
const FAKE: &str = r#"
dialled = 0
local function fake(reply)
  return { connect = function()
    dialled = dialled + 1
    local h = { left = reply }
    function h:send() return true end
    function h:recv(n) local out = self.left:sub(1, n); self.left = self.left:sub(n + 1); return out end
    function h:close() return true end
    return h
  end }
end
"#;

/// What `probe` leaves in `casper.answer`, run after [`FAKE`].
fn answer(probe: &str) -> String {
    let mut engine = Engine::new();
    engine
        .run(&format!("{FAKE}\n{probe}"), "probe.lua")
        .expect("the probe runs");
    engine.harvest();
    engine
        .setting("answer")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[test]
fn a_reply_frame_over_the_cap_or_empty_is_refused_before_it_is_read() {
    for client in ["hexe", "oslo"] {
        for header in ["1, 0, 0, 1", "0, 0, 0, 0"] {
            let said = answer(&format!(
                "local lib = load(casper.clients.{client})(fake(string.char({header})))
                 local s = lib.connect({{ path = '/nowhere' }})
                 local ok, why = s:call('verbs')
                 casper.answer = tostring(ok) .. '|' .. tostring(why)"
            ));
            assert!(said.starts_with("nil|"), "{client} {header}: {said}");
            assert!(
                said.contains("which is not one"),
                "{client} {header}: {said}"
            );
        }
    }
}

#[test]
fn a_call_over_the_cap_is_refused_before_it_is_sent() {
    for client in ["hexe", "oslo"] {
        let said = answer(&format!(
            "local lib = load(casper.clients.{client})(fake(''))
             local s = lib.connect({{ path = '/nowhere' }})
             local before = dialled
             local ok, why = s:call('send', string.rep('x', 16777217))
             casper.answer = tostring(ok) .. '|' .. tostring(why) .. '|' .. (dialled - before)"
        ));
        assert!(said.starts_with("nil|"), "{client}: {said}");
        assert!(said.contains("frame cap"), "{client}: {said}");
        assert!(
            said.ends_with("|0"),
            "{client} dialled for an oversized call: {said}"
        );
    }
}

#[test]
fn no_socket_path_is_built_from_dollar_uid() {
    let dir = Scratch::new("clients", "uid");
    let config = dir.join("casper");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::write(
        config.join("tools.lua"),
        r#"casper.tool("where", { run = function()
             local hexe = load(casper.clients.hexe)(casper.stream)
             local oslo = load(casper.clients.oslo)(casper.stream)
             return tostring(hexe.where("x")) .. " " .. tostring(oslo.where("x"))
           end })"#,
    )
    .expect("declaration");
    let done = Command::new(env!("CARGO_BIN_EXE_casper"))
        .arg("run")
        .env("XDG_CONFIG_HOME", &*dir)
        .env("XDG_DATA_HOME", &*dir)
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("HEXE_API_SOCKET")
        .env_remove("OSLO_SOCK")
        .env("UID", "4242")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut casper| {
            use std::io::Write as _;
            let mut stdin = casper.stdin.take().expect("stdin");
            stdin.write_all(br#"{"tool":"where","args":{}}"#)?;
            drop(stdin);
            casper.wait_with_output()
        })
        .expect("casper ran");
    let reply: serde_json::Value = serde_json::from_slice(&done.stdout).expect("a reply");
    assert_eq!(reply["result"][0]["said"], "nil nil", "{reply}");
}
