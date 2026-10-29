#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::{scratch::Scratch, tools::Ran};
use serde_json::{Value, json};
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EDIT_LIMIT: usize = 1024 * 1024;

#[path = "support/native_namespace.rs"]
mod native_namespace;
use native_namespace::NativeNamespace;

struct Owned(Child);

impl Owned {
    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.stop();
    }
}

fn program(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("fixture PATH"))
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("required fixture program missing: {name}"))
}

struct Fixture {
    dir: Scratch,
    namespace: NativeNamespace,
    generation: u64,
    path: std::ffi::OsString,
    socket: Option<UnixStream>,
    server: Option<Owned>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.socket.take();
        self.server.take();
    }
}

impl Fixture {
    fn new(bubblewrap: bool, socket: bool) -> Self {
        let dir = Scratch::new("ce", "source");
        for name in [
            "work",
            "home",
            "config/casper",
            "runtime",
            "data",
            "state",
            "cache",
            "tmp",
            "bin",
        ] {
            std::fs::create_dir_all(dir.join(name)).expect("fixture directory");
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("runtime"), std::fs::Permissions::from_mode(0o700))
            .expect("private runtime");
        std::fs::write(
            dir.join("config/casper/tools.lua"),
            include_str!("../config/tools.lua"),
        )
        .expect("shipped tools");
        std::fs::create_dir_all(dir.join("config/casper/plugin")).expect("plugin directory");
        std::fs::write(
            dir.join("config/casper/plugin/probes.lua"),
            r#"
casper.tool("source_probe", {
  description = "Read a synthetic source through its native reader",
  parameters = {type="object", properties={path={type="string"},measure={type="boolean"}}}, needs="read",
  run = function(args)
    local text, why = casper.source(args.path)
    if text and args.measure then
      return {said=casper.json.encode({bytes=#text, first=text:byte(1), last=text:byte(-1)})}
    end
    return {said=text or why, failed=text == nil}
  end,
})
casper.tool("capture_probe", {
  description = "Inspect bounded ordinary command output",
  parameters = {type="object", properties={}}, needs="run",
  run = function()
    local done = casper.exec("sh", {"-c", "head -c 600000 /dev/zero | tr '\\0' x"})
    return {said=casper.json.encode({code=done.code, bytes=#done.out, tail=done.out:sub(-100)})}
  end,
})
casper.tool("write_probe", {
  description = "Replace synthetic source through its native writer",
  parameters = {type="object", properties={path={type="string"}}}, needs="write",
  run = function(args)
    local ok, why = casper.replace_source(args.path, "new")
    return {said=why or "stored", failed=not ok}
  end,
})
"#,
        )
        .expect("probe declarations");
        let mut paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .filter(|path| !path.join("bwrap").is_file())
            .collect();
        if bubblewrap {
            let backend = program("bwrap");
            paths.insert(
                0,
                backend
                    .parent()
                    .expect("captured backend parent")
                    .to_owned(),
            );
        }
        let namespace = NativeNamespace::new(&dir, &dir.join("work"));
        let mut fixture = Self {
            dir,
            namespace,
            generation: 0,
            path: std::env::join_paths(paths).expect("fixture PATH"),
            socket: None,
            server: None,
        };
        let probe = fixture.call(
            "shell",
            json!({"command":"printf SOURCE_ACQUISITION_READY"}),
        );
        assert!(
            !probe.failed && probe.said.contains("SOURCE_ACQUISITION_READY"),
            "jail prerequisite: {}",
            probe.said
        );
        if socket {
            fixture.connect();
        }
        fixture
    }

    fn command(&self, bounded: bool) -> Command {
        let binary = std::env::var_os("CASPER_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_casper").into());
        let mut command = if bounded {
            let mut command = self.namespace.command(program("timeout"));
            command.args(["--kill-after=2s", "45s"]).arg(&binary);
            command
        } else {
            self.namespace.command(&binary)
        };
        command
            .current_dir(self.dir.join("work"))
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", self.dir.join("home"))
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .env("XDG_RUNTIME_DIR", self.dir.join("runtime"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("TMPDIR", self.dir.join("tmp"))
            .env("CASPER_JAIL", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn connect(&mut self) {
        let path = self.dir.join("runtime/casper/s");
        let mut server = Owned(
            self.command(false)
                .args(["serve", "--at"])
                .arg(&path)
                .stdout(Stdio::null())
                .spawn()
                .expect("server"),
        );
        server
            .0
            .stdin
            .take()
            .expect("server key")
            .write_all(format!("{}\n", "5e".repeat(32)).as_bytes())
            .expect("key");
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            if let Ok(stream) = UnixStream::connect(&path) {
                break stream;
            }
            assert!(server.0.try_wait().expect("server status").is_none());
            assert!(Instant::now() < deadline, "socket readiness");
            std::thread::sleep(Duration::from_millis(10));
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(40)))
            .expect("read deadline");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("write deadline");
        self.socket = Some(stream);
        self.server = Some(server);
    }

    fn call(&mut self, tool: &str, args: Value) -> Ran {
        let mut call = json!({"tool":tool,"args":args});
        if tool == "read" || tool == "edit" {
            call["file_revision"] = json!({"version":1,"operation":tool});
        }
        if tool == "edit" {
            let observed = self.raw(json!({"tool":"read","args":{"path":call["args"]["path"]},
                "file_revision":{"version":1,"operation":"read"}}));
            if let Some(expected) = observed["result"][0]["observations"][0]["revision"].as_str() {
                self.generation = self
                    .generation
                    .checked_add(1)
                    .expect("fixture mutation generation");
                call["revision_contract"] = json!(1);
                call["expected_revision"] = json!(expected);
                call["mutation_id"] = json!(format!("source-{}", self.generation));
            } else {
                let refused = self.raw(call);
                assert_eq!(
                    refused["ok"], false,
                    "edit without a receipt ran: {refused}"
                );
                let why = observed["result"][0]["said"]
                    .as_str()
                    .or_else(|| observed["error"].as_str())
                    .unwrap_or("no editable observation");
                return Ran::failed(format!("native read supplied no receipt: {why}"));
            }
        }
        let reply = self.raw(call);
        if tool == "edit" && reply["ok"] == false {
            return Ran::failed(reply["error"].as_str().expect("native guard refusal"));
        }
        assert_eq!(reply["ok"], true, "{reply}");
        serde_json::from_value(reply["result"][0].clone()).expect("tool result")
    }

    fn raw(&mut self, call: Value) -> Value {
        self.raw_with(call, false)
    }

    fn raw_with(&mut self, call: Value, cbor: bool) -> Value {
        if let Some(stream) = self.socket.as_mut() {
            let request = json!({"call":"run","key":"5e".repeat(32),"args":[call]});
            let body = if cbor {
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(&request, &mut bytes).expect("CBOR request");
                bytes
            } else {
                serde_json::to_vec(&request).expect("JSON request")
            };
            casper::framing::write_frame(stream, &body).expect("send");
            let body = casper::framing::read_frame(stream).expect("reply");
            casper::framing::decode(&body).expect("socket reply")
        } else {
            let mut child = Owned(
                self.command(true)
                    .args(["run", if cbor { "--cbor" } else { "--json" }])
                    .spawn()
                    .expect("tool process"),
            );
            child
                .0
                .stdin
                .take()
                .expect("stdin")
                .write_all(serde_json::to_string(&call).expect("request").as_bytes())
                .expect("send");
            let output = child.0.stdout.take().expect("output");
            let errors = child.0.stderr.take().expect("errors");
            let reader = std::thread::spawn(move || {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                output
                    .take(4 * 1024 * 1024)
                    .read_to_end(&mut bytes)
                    .expect("reply bytes");
                bytes
            });
            let stderr = std::thread::spawn(move || {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                errors
                    .take(64 * 1024)
                    .read_to_end(&mut bytes)
                    .expect("stderr");
                bytes
            });
            let status = child.0.wait().expect("tool status");
            let bytes = reader.join().expect("reader");
            let errors = stderr.join().expect("stderr reader");
            assert!(
                status.success(),
                "tool process failed: {}",
                String::from_utf8_lossy(&errors)
            );
            casper::framing::decode(&bytes).expect("CLI reply")
        }
    }

    fn edit(&mut self, before: &[u8], old: &str, new: &str) -> (Ran, Vec<u8>) {
        self.edit_named("source.txt", before, old, new)
    }

    fn edit_named(&mut self, name: &str, before: &[u8], old: &str, new: &str) -> (Ran, Vec<u8>) {
        let path = self.dir.join("work").join(name);
        std::fs::write(&path, before).expect("source fixture");
        let result = self.call("edit", json!({"path":name,"old":old,"new":new}));
        let after = std::fs::read(path).expect("source after operation");
        (result, after)
    }
}

fn complete_source(bubblewrap: bool, socket: bool) {
    let mut fixture = Fixture::new(bubblewrap, socket);
    let (ran, after) = fixture.edit(b"old\nsmall tail\n", "old", "new");
    assert!(
        !ran.failed,
        "small edit did not reach acquisition: {}",
        ran.said
    );
    assert_eq!(after, b"new\nsmall tail\n");
    for size in [casper::running::MOST, casper::running::MOST + 37] {
        let mut original = b"old\n".to_vec();
        while original.len() < size {
            original.extend_from_slice(b"padding padding padding\n");
        }
        original.extend_from_slice(b"DISTINCT_FINAL_SENTINEL\n");
        let (ran, actual) = fixture.edit(&original, "old", "new");
        let mut expected = original.clone();
        expected.splice(..3, b"new".iter().copied());
        assert!(!ran.failed, "bounded valid source was refused");
        assert!(actual == expected, "large edit lost source bytes");
    }

    let mut unicode = b"old\r\n".to_vec();
    while unicode.len() < casper::running::MOST - 1 {
        unicode.extend_from_slice(b"padding\r\n");
    }
    unicode.truncate(casper::running::MOST - 1);
    unicode.extend_from_slice("€\r\nno trailing newline".as_bytes());
    let (ran, after) = fixture.edit(&unicode, "old", "new");
    let mut expected = unicode.clone();
    expected.splice(..3, b"new".iter().copied());
    assert!(
        !ran.failed && after == expected,
        "Unicode or line ending bytes changed"
    );

    let mut near_limit = b"old\n".to_vec();
    while near_limit.len() < EDIT_LIMIT {
        near_limit.extend_from_slice(b"padding\n");
    }
    near_limit.truncate(EDIT_LIMIT - 1);
    let (ran, after) = fixture.edit(&near_limit, "old", "new");
    near_limit.splice(..3, b"new".iter().copied());
    assert!(
        !ran.failed && after == near_limit,
        "source below the edit limit was not preserved"
    );

    let marker = "old\n… 337856 more bytes, not shown\n";
    let (ran, after) = fixture.edit(marker.as_bytes(), "old", "new");
    assert!(!ran.failed && after == marker.replacen("old", "new", 1).as_bytes());

    for (before, old, new) in [
        (&b"old\n"[..], "", "new"),
        (&b"old\n"[..], "absent", "new"),
        (&b"old old\n"[..], "old", "new"),
        (&b"old\n"[..], "old", "new\0data"),
        (&b"old\0binary\n"[..], "old", "new"),
        (&b"old\xff\n"[..], "old", "new"),
    ] {
        let (ran, after) = fixture.edit(before, old, new);
        assert!(
            ran.failed && after == before,
            "invalid edit was not an unchanged refusal"
        );
    }
    for length in [EDIT_LIMIT, EDIT_LIMIT + 1] {
        let mut before = b"old\n".to_vec();
        before.resize(length, b'x');
        let (ran, after) = fixture.edit(&before, "old", "new");
        assert!(
            ran.failed && after == before,
            "edit at/above the byte limit changed source"
        );
    }
    let (ran, after) = fixture.edit(b"old", "old", &"x".repeat(EDIT_LIMIT));
    assert!(
        ran.failed && after == b"old",
        "oversized result changed source"
    );
    for name in ["--cbor", "-", "spaces and 'quotes'.txt"] {
        let (ran, after) = fixture.edit_named(name, b"old\n", "old", "new");
        assert!(
            !ran.failed && after == b"new\n",
            "filename changed acquisition semantics"
        );
    }
    let missing = fixture.call("edit", json!({"path":"missing","old":"old","new":"new"}));
    assert!(missing.failed && !fixture.dir.join("work/missing").exists());
    std::fs::create_dir(fixture.dir.join("work/directory")).expect("directory source");
    let directory = fixture.call("edit", json!({"path":"directory","old":"old","new":"new"}));
    assert!(directory.failed && fixture.dir.join("work/directory").is_dir());

    let outside = fixture.dir.join("outside-source");
    std::fs::write(&outside, "SYNTHETIC_SOURCE_SECRET").expect("denied source");
    let denied = fixture.call("source_probe", json!({"path":outside}));
    assert!(
        denied.failed && !denied.said.contains("SYNTHETIC_SOURCE_SECRET"),
        "source primitive bypassed the jail"
    );
    let denied = fixture.call("write_probe", json!({"path":outside}));
    assert!(denied.failed, "replacement primitive bypassed the jail");
    assert_eq!(
        std::fs::read(&outside).expect("denied target"),
        b"SYNTHETIC_SOURCE_SECRET"
    );

    let captured = fixture.call("capture_probe", json!({}));
    assert!(!captured.failed);
    let captured: Value = serde_json::from_str(&captured.said).expect("capture metadata");
    assert_eq!(captured["code"], 0);
    assert!(captured["bytes"].as_u64().expect("byte count") <= (casper::running::MOST + 64) as u64);
    assert!(
        captured["tail"]
            .as_str()
            .expect("output tail")
            .ends_with("337856 more bytes, not shown")
    );
}

#[test]
fn complete_source_command_landlock() {
    complete_source(false, false);
}
#[test]
fn complete_source_socket_landlock() {
    complete_source(false, true);
}
#[test]
fn complete_source_command_bubblewrap() {
    complete_source(true, false);
}
#[test]
fn complete_source_socket_bubblewrap() {
    complete_source(true, true);
}

#[test]
fn invalid_utf8_is_not_transformed_lossily() {
    let mut fixture = Fixture::new(false, false);
    let original = b"old\ninvalid: \xff\n";
    let (ran, after) = fixture.edit(original, "old", "new");
    assert!(
        ran.failed && after == original,
        "invalid text was transformed"
    );
}

#[test]
fn edit_limit_is_an_unchanged_refusal() {
    let mut fixture = Fixture::new(false, false);
    let mut original = b"old\n".to_vec();
    original.resize(EDIT_LIMIT, b'x');
    let (ran, after) = fixture.edit(&original, "old", "new");
    assert!(
        ran.failed && after == original,
        "oversized edit changed source"
    );
}

fn workers(work: &Path) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .expect("processes")
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            (std::fs::read_link(entry.path().join("cwd")).ok()?.as_path() == work).then_some(pid)
        })
        .take(32)
        .collect()
}

fn cancelled_source(bubblewrap: bool, socket: bool) {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
    let mut fixture = Fixture::new(bubblewrap, socket);
    let path = fixture.dir.join("work/blocked");
    assert!(
        Command::new(program("mkfifo"))
            .arg(&path)
            .status()
            .expect("FIFO")
            .success()
    );
    let inode = std::fs::metadata(&path).expect("FIFO metadata").ino();
    let call = json!({"tool":"source_probe","args":{"path":"blocked"}});
    let mut caller = if let Some(stream) = fixture.socket.as_mut() {
        casper::framing::write_frame(
            stream,
            &serde_json::to_vec(&json!({
                "call":"run","key":"5e".repeat(32),"args":[call]
            }))
            .expect("request"),
        )
        .expect("send blocked call");
        None
    } else {
        let mut caller = Owned(
            fixture
                .command(false)
                .arg("run")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("caller"),
        );
        caller
            .0
            .stdin
            .take()
            .expect("stdin")
            .write_all(serde_json::to_string(&call).expect("request").as_bytes())
            .expect("send");
        Some(caller)
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut writer = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(writer) => break writer,
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::ENXIO), "FIFO readiness"),
        }
        assert!(
            Instant::now() < deadline,
            "source helper did not open the FIFO"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    writer.write_all(b"old\n").expect("partial source");
    let work = fixture.dir.join("work");
    let native_reader = workers(&work).into_iter().any(|pid| {
        use std::io::Read as _;
        let Ok(file) = std::fs::File::open(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let mut args = Vec::new();
        file.take(4096).read_to_end(&mut args).expect("worker args");
        args.split(|byte| *byte == 0)
            .any(|arg| arg == b"--source-text")
    });
    assert!(
        native_reader,
        "cancellation did not reach the actual jailed source reader"
    );
    if let Some(caller) = caller.as_mut() {
        caller.stop();
    }
    if let Some(server) = fixture.server.as_mut() {
        server.stop();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !workers(&work).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let leaked = workers(&work);
    for pid in &leaked {
        if let Some(pid) = rustix::process::Pid::from_raw(*pid as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
    assert!(
        leaked.is_empty(),
        "cancelled source acquisition left workers alive"
    );
    let metadata = std::fs::metadata(&path).expect("source after cancellation");
    assert!(
        metadata.file_type().is_fifo() && metadata.ino() == inode,
        "cancelled acquisition replaced the target"
    );
}

#[test]
fn cancelled_source_acquisition_reaps_workers_without_mutation() {
    for bubblewrap in [false, true] {
        for socket in [false, true] {
            cancelled_source(bubblewrap, socket);
        }
    }
}

#[test]
fn worst_case_json_escaping_preserves_the_complete_source() {
    let text = vec![1_u8; EDIT_LIMIT - 1];
    for bubblewrap in [false, true] {
        for socket in [false, true] {
            let mut fixture = Fixture::new(bubblewrap, socket);
            std::fs::write(fixture.dir.join("work/escaped"), &text).expect("escaped source");
            let ran = fixture.call("source_probe", json!({"path":"escaped","measure":true}));
            assert!(!ran.failed, "bounded source transport was truncated");
            let metadata: Value = serde_json::from_str(&ran.said).expect("source metadata");
            assert_eq!(metadata, json!({"bytes":EDIT_LIMIT-1,"first":1,"last":1}));
        }
    }
}

#[test]
fn a_missing_complete_source_capability_fails_closed() {
    for bubblewrap in [false, true] {
        for socket in [false, true] {
            for capability in ["source", "replace_source"] {
                let mut fixture = Fixture::new(bubblewrap, socket);
                std::fs::create_dir_all(fixture.dir.join("config/casper/after/plugin"))
                    .expect("late layer");
                std::fs::write(
                    fixture.dir.join("config/casper/after/plugin/source.lua"),
                    format!("casper.{capability} = nil"),
                )
                .expect("missing capability");
                std::fs::write(fixture.dir.join("work/source.txt"), b"old\n").expect("source");
                let ran = fixture.call(
                    if capability == "source" {
                        "source_probe"
                    } else {
                        "write_probe"
                    },
                    json!({"path":"source.txt"}),
                );
                let after = std::fs::read(fixture.dir.join("work/source.txt")).expect("unchanged");
                assert!(
                    ran.failed && after == b"old\n",
                    "missing source capability did not refuse unchanged"
                );
                let (native, after) = fixture.edit(b"old\n", "old", "new");
                assert!(
                    !native.failed && after == b"new\n",
                    "native edit depended on legacy export: {}",
                    native.said
                );
            }
        }
    }
}

#[test]
fn edit_io_does_not_depend_on_a_cat_selected_from_path() {
    for bubblewrap in [false, true] {
        for socket in [false, true] {
            let mut fixture = Fixture::new(bubblewrap, false);
            let readers = fixture.dir.join("work/.header-reader");
            std::fs::create_dir(&readers).expect("jailed header-reader directory");
            std::os::unix::fs::symlink(program("head"), readers.join("head"))
                .expect("image-header reader without cat");
            fixture.path = std::env::join_paths(std::iter::once(readers).chain(
                std::env::split_paths(&fixture.path).filter(|path| !path.join("cat").is_file()),
            ))
            .expect("PATH without cat");
            assert!(!std::env::split_paths(&fixture.path).any(|path| path.join("cat").is_file()));
            if socket {
                fixture.connect();
            }
            let (ran, after) = fixture.edit(b"old\nTAIL\n", "old", "new");
            assert!(
                !ran.failed && after == b"new\nTAIL\n",
                "native edit IO depended on cat or truncated the target: {}",
                ran.said
            );
        }
    }
}

#[test]
fn revision_preconditions_are_never_ignored_by_either_execution_door() {
    for bubblewrap in [false, true] {
        for socket in [false, true] {
            for cbor in [false, true] {
                let mut fixture = Fixture::new(bubblewrap, socket);
                let path = fixture.dir.join("work/guarded.txt");
                let evaluated = fixture.dir.join("work/config-evaluated");
                std::fs::write(&path, b"old\nTAIL\n").expect("source");
                let observed = fixture.call("read", json!({"path":"guarded.txt"}));
                let expected = observed
                    .files
                    .observations
                    .first()
                    .expect("native receipt")
                    .revision
                    .clone();
                std::fs::write(
                    fixture.dir.join("config/casper/plugin/guard-sentinel.lua"),
                    r#"casper.exec("sh", {"-c", "printf loaded > config-evaluated"})"#,
                )
                .expect("configuration side effect probe");
                for fields in [
                    json!({"revision_contract":1,"expected_revision":"unknown-v1","mutation_id":"m-1"}),
                    json!({"revision_contract":99}),
                    json!({"revision_contract":null}),
                    json!({"expected_revision":"unknown-v1"}),
                    json!({"expected_revision":null}),
                    json!({"mutation_id":"m-1"}),
                    json!({"mutation_id":null}),
                ] {
                    let mut call = json!({"tool":"edit","args":{"path":"guarded.txt","old":"old","new":"new"}});
                    call.as_object_mut()
                        .expect("call")
                        .extend(fields.as_object().expect("metadata").clone());
                    let reply = fixture.raw_with(call, cbor);
                    assert_eq!(
                        std::fs::read(&path).expect("source"),
                        b"old\nTAIL\n",
                        "ignored {fields}: {reply}"
                    );
                    assert_eq!(reply["ok"], false, "guard was silently dropped: {fields}");
                    assert!(
                        reply["error"]
                            .as_str()
                            .expect("refusal")
                            .contains("revision")
                    );
                    assert!(
                        !evaluated.exists(),
                        "guard rejection evaluated configuration"
                    );
                }
                let rejected = fixture.raw(json!({"tool":"edit","args":{"path":"guarded.txt","old":"old","new":"new"},
                    "file_revision":{"version":1,"operation":"edit"},
                    "revision_contract":1,"expected_revision":expected,"mutation_id":"effectful-declaration"}));
                assert!(
                    rejected["ok"] == false && !evaluated.exists(),
                    "native admission ran configuration effects"
                );
                assert!(
                    rejected["error"]
                        .as_str()
                        .expect("admission refusal")
                        .contains("effectful")
                );
                let legacy = fixture.call("source_probe", json!({"path":"guarded.txt"}));
                assert!(!legacy.failed);
                assert_eq!(
                    std::fs::read(&evaluated).expect("legacy configuration executed"),
                    b"loaded"
                );
                std::fs::remove_file(fixture.dir.join("config/casper/plugin/guard-sentinel.lua"))
                    .expect("remove effectful fixture layer");
                let (result, after) = fixture.edit(b"old\nTAIL\n", "old", "new");
                assert!(!result.failed);
                assert_eq!(after, b"new\nTAIL\n");
            }
        }
    }
}
