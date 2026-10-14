//! The hexe and oslo client libraries casper lends a declaration: the family's frame cap on both
//! directions, and no path built from `$UID`.

use casper::lua::engine::Engine;
use casper::scratch::Scratch;
use std::process::{Command, Stdio};

/// A transport whose every connection answers `reply`, counting how often it was dialled, the
/// most one `recv` asked for, and the last connection, which `close` marks closed.
const FAKE: &str = r#"
dialled, largest, last = 0, 0, nil
local function fake(reply)
  return { connect = function()
    dialled = dialled + 1
    local h = { left = reply, closed = false }
    last = h
    function h:send() return true end
    function h:recv(n)
      largest = math.max(largest, n)
      local out = self.left:sub(1, n); self.left = self.left:sub(n + 1); return out
    end
    function h:close() self.closed = true; return true end
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

/// The family's wire corpus, `tests/wire-corpus/cases.json`.
fn corpus() -> serde_json::Value {
    let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wire-corpus");
    let cases: serde_json::Value =
        serde_json::from_slice(&std::fs::read(at.join("cases.json")).expect("cases"))
            .expect("parse");
    let mut client = cases["client"].clone();
    for case in client.as_array_mut().expect("client cases") {
        let bytes = std::fs::read(at.join(case["file"].as_str().expect("file"))).expect("case");
        case["bytes"] = bytes.into();
    }
    client
}

#[test]
fn every_client_case_of_the_wire_corpus_is_taken_as_it_says() {
    for client in ["hexe", "oslo"] {
        for case in corpus().as_array().expect("cases") {
            let bytes: Vec<String> = case["bytes"]
                .as_array()
                .expect("bytes")
                .iter()
                .map(ToString::to_string)
                .collect();
            let said = answer(&format!(
                "local lib = load(casper.clients.{client})(fake(string.char({})))
                 local s = lib.connect({{ path = '/nowhere' }})
                 local raised, value, why = pcall(s.call, s, 'verbs')
                 casper.answer = table.concat({{ tostring(raised), tostring(value), tostring(why),
                   tostring(last.closed), tostring(largest) }}, '|')",
                bytes.join(", ")
            ));
            let took: Vec<&str> = said.split('|').collect();
            let (file, expect) = (&case["file"], case["expect"].as_str().unwrap_or_default());
            assert_eq!(took[0], "true", "{client} {file} raised: {said}");
            match expect {
                "accepted" => assert_eq!(took[1], "x", "{client} {file}: {said}"),
                "failed" => assert_eq!(took[1..3], ["nil", "no"], "{client} {file}: {said}"),
                "refused" => {
                    assert_eq!(took[1], "nil", "{client} {file}: {said}");
                    assert!(took[2].contains("revision 2"), "{client} {file}: {said}");
                }
                "error_and_close" | "error_and_close_without_allocating" => {
                    assert_eq!(took[1], "nil", "{client} {file}: {said}");
                    assert_eq!(took[3], "true", "{client} {file} left open: {said}");
                    let largest: u64 = took[4].parse().expect("a count");
                    assert!(
                        expect == "error_and_close" || largest <= 4,
                        "{client} {file} asked for {largest} bytes: {said}"
                    );
                }
                other => panic!("{file}: no such expectation '{other}'"),
            }
        }
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
