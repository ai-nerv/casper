//! The family's wire corpus against the real `casper serve`: every server case answered as
//! `cases.json` says, and the connection left open or closed as it says.
//!
//! The socket answers `tools` and `run`, and only with the key `serve` was started with. So a case
//! whose body decodes as a call is re-framed in its own encoding with the key added and `verbs`
//! asked as `tools`; every other case goes as it is.

use casper::framing::{self, MOST, Wire};
use casper::scratch::Scratch;
use std::io::{ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn key() -> String {
    "c0".repeat(32)
}

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/wire-corpus")
}

fn cases() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(corpus().join("cases.json")).expect("cases"))
        .expect("parse")
}

/// A `serve` of this test's own in a private runtime directory, ended when the test ends.
struct Serving {
    child: Child,
    socket: PathBuf,
    _dir: Scratch,
}

impl Serving {
    fn start() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let dir = Scratch::new("cwc", "s");
        let run = dir.join("r");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&run)
            .expect("runtime");
        let config = dir.join("c/casper");
        std::fs::create_dir_all(&config).expect("config");
        std::fs::write(
            config.join("tools.lua"),
            r#"casper.tool("corpus", { description = "nothing", run = function() return "" end })"#,
        )
        .expect("declaration");
        let socket = run.join("casper/wc.sock");
        let mut child = Command::new(env!("CARGO_BIN_EXE_casper"))
            .args(["serve", "--at"])
            .arg(&socket)
            .env("XDG_RUNTIME_DIR", &run)
            .env("XDG_CONFIG_HOME", dir.join("c"))
            .env("XDG_DATA_HOME", dir.join("d"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("serve");
        let mut stdin = child.stdin.take().expect("serve's stdin");
        stdin
            .write_all(format!("{}\n", key()).as_bytes())
            .expect("the key");
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&socket).is_err() {
            assert!(child.try_wait().expect("status").is_none(), "serve exited");
            assert!(Instant::now() < deadline, "serve did not bind");
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            child,
            socket,
            _dir: dir,
        }
    }

    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a deadline");
        stream
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `bytes` as casper's socket takes it: a whole frame whose body is a call gets the key, and
/// `verbs` becomes `tools`, in the body's own encoding. Anything else is returned as it is.
fn keyed(bytes: &[u8]) -> Vec<u8> {
    let Some((head, body)) = bytes.split_first_chunk::<4>() else {
        return bytes.to_vec();
    };
    if body.is_empty() || u32::from_be_bytes(*head) as usize != body.len() {
        return bytes.to_vec();
    }
    let Ok(serde_json::Value::Object(mut call)) = framing::decode(body) else {
        return bytes.to_vec();
    };
    if call.get("call").and_then(|c| c.as_str()) == Some("verbs") {
        call.insert("call".into(), "tools".into());
    }
    call.insert("key".into(), key().into());
    let body = Wire::of(body).encode(&call).expect("encode");
    let mut framed = u32::try_from(body.len())
        .expect("fits")
        .to_be_bytes()
        .to_vec();
    framed.extend_from_slice(&body);
    framed
}

fn send(stream: &mut UnixStream, bytes: &[u8], bytewise: bool) {
    if !bytewise {
        stream.write_all(bytes).expect("write");
        return;
    }
    for byte in bytes {
        stream.write_all(&[*byte]).expect("write");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn decoded(body: &[u8]) -> (serde_json::Value, &'static str) {
    let value = framing::decode(body).expect("a reply in one of the two encodings");
    let encoding = match Wire::of(body) {
        Wire::Cbor => "cbor",
        Wire::Json => "json",
    };
    (value, encoding)
}

#[test]
fn every_server_case_of_the_corpus_is_answered_as_it_says() {
    let cases = cases();
    let verbs = keyed(&std::fs::read(corpus().join("server/verbs-json.frame")).expect("verbs"));
    let serving = Serving::start();
    for case in cases["server"].as_array().expect("server cases") {
        let file = case["file"].as_str().expect("file");
        let bytes = keyed(&std::fs::read(corpus().join(file)).expect("case"));
        let mut stream = serving.connect();
        send(&mut stream, &bytes, case["split"] == "bytewise");
        let body = framing::read_frame(&mut stream).unwrap_or_else(|e| panic!("{file}: {e}"));
        let (said, encoding) = decoded(&body);
        let expect = &case["expect"];
        assert_eq!(said["ok"], expect["ok"], "{file}: {said}");
        if let Some(wanted) = expect["encoding"].as_str() {
            assert_eq!(encoding, wanted, "{file}");
        }
        if let Some(fault) = expect["fault"].as_str() {
            assert_eq!(said["fault"], fault, "{file}: {said}");
        }
        if let Some(part) = expect["error_contains"].as_str() {
            let error = said["error"].as_str().unwrap_or_default();
            assert!(error.contains(part), "{file}: {said}");
        }
        match case["then"].as_str() {
            Some("open") => {
                send(&mut stream, &verbs, false);
                let next = framing::read_frame(&mut stream)
                    .unwrap_or_else(|e| panic!("{file}: closed: {e}"));
                assert_eq!(decoded(&next).0["ok"], true, "{file}: out of step after");
            }
            Some("closed") => {
                let after = framing::read_frame(&mut stream).map(|_| ()).err();
                let kind = after.map(|e| e.kind());
                assert_eq!(kind, Some(ErrorKind::UnexpectedEof), "{file}: left open");
            }
            _ => {}
        }
    }
}

#[test]
fn a_corpus_call_as_it_is_carries_no_key_and_is_refused() {
    let serving = Serving::start();
    for file in ["server/verbs-json.frame", "server/verbs-cbor.frame"] {
        let bytes = std::fs::read(corpus().join(file)).expect("case");
        let mut stream = serving.connect();
        send(&mut stream, &bytes, false);
        let (said, _) = decoded(&framing::read_frame(&mut stream).expect("a reply"));
        assert_eq!(said["fault"], "refused", "{file}: {said}");
        let error = said["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("the key it was started with"),
            "{file}: {said}"
        );
    }
}

#[test]
fn the_corpus_names_the_cap_this_program_reads_at() {
    assert_eq!(cases()["cap"].as_u64(), u64::try_from(MOST).ok());
}
