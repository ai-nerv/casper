#![allow(clippy::expect_used, clippy::indexing_slicing)]

use casper::scratch::Scratch;
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "file_worker/admission.rs"]
mod admission;
#[path = "file_worker/transactions.rs"]
mod transactions;

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|path| path.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("missing prerequisite: {name}"))
}

struct Fixture {
    root: Scratch,
    path: std::ffi::OsString,
    launcher: PathBuf,
    binary: PathBuf,
}

impl Fixture {
    fn new(bubblewrap: bool) -> Self {
        let root = Scratch::new("native-file", "snapshot");
        for name in [
            "work", "home", "config", "data", "state", "cache", "tmp", "var-tmp", "bin",
        ] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(name))
                .expect("private fixture");
        }
        std::fs::set_permissions(
            root.join("var-tmp"),
            std::fs::Permissions::from_mode(0o1777),
        )
        .expect("isolated sticky anchor");
        std::fs::create_dir(root.join("home/.ssh")).expect("fixture credential store");
        std::fs::write(root.join("home/.ssh/id"), b"FIXTURE_SECRET").expect("fixture secret");
        let launcher = executable("bwrap");
        let metadata = std::fs::metadata(&launcher).expect("native recipe backend");
        assert_eq!(
            metadata.mode() & 0o222,
            0,
            "test backend must be captured read-only before parallel tests"
        );
        assert_eq!(metadata.nlink(), 1, "test backend has hardlink aliases");
        let mut paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .filter(|path| !path.join("bwrap").is_file())
            .collect();
        if bubblewrap {
            paths.insert(0, launcher.parent().expect("backend parent").to_owned());
        }
        let binary = std::env::var_os("CASPER_TEST_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_casper")));
        Self {
            root,
            path: std::env::join_paths(paths).expect("backend PATH"),
            launcher,
            binary,
        }
    }

    fn command(&self, jailed: bool) -> Command {
        self.command_mask(jailed, None)
    }

    fn command_mask(&self, jailed: bool, mask: Option<&str>) -> Command {
        self.command_program(jailed, mask, &self.binary)
    }

    fn command_program(
        &self,
        jailed: bool,
        mask: Option<&str>,
        program: &std::path::Path,
    ) -> Command {
        self.command_program_with_work_limit(jailed, mask, program, None)
    }

    fn command_program_with_work_limit(
        &self,
        jailed: bool,
        mask: Option<&str>,
        program: &std::path::Path,
        size: Option<u64>,
    ) -> Command {
        let mut command = Command::new(&self.launcher);
        command
            .args([
                "--unshare-user",
                "--unshare-pid",
                "--uid",
                "0",
                "--gid",
                "0",
                "--ro-bind",
                "/",
                "/",
            ])
            .args(["--dev", "/dev", "--proc", "/proc"])
            .args(["--cap-add", "ALL"]);
        if let Some(size) = size {
            command
                .args(["--size", &size.to_string(), "--tmpfs"])
                .arg(self.root.join("work"));
        } else {
            command
                .arg("--bind")
                .arg(self.root.join("work"))
                .arg(self.root.join("work"));
        }
        command
            .arg("--bind")
            .arg(self.root.join("tmp"))
            .arg(self.root.join("tmp"))
            .arg("--bind")
            .arg(self.root.join("var-tmp"))
            .arg("/var/tmp")
            .args(["--die-with-parent", "--new-session", "--chdir"])
            .arg(self.root.join("work"))
            .arg("--");
        if let Some(mask) = mask {
            command.arg(executable("sh")).args([
                "-c",
                "umask \"$1\"; shift; exec \"$@\"",
                "fixture-umask",
                mask,
            ]);
        }
        command
            .arg(program)
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("CASPER_JAIL", if jailed { "1" } else { "" })
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn raw(&self, jailed: bool, args: &[&str]) -> Value {
        self.run(self.command(jailed), args)
    }

    fn run(&self, command: Command, args: &[&str]) -> Value {
        self.run_input(command, args, None)
    }

    fn run_input(&self, command: Command, args: &[&str], input: Option<Vec<u8>>) -> Value {
        self.run_input_format(command, args, input, false)
    }

    fn run_input_format(
        &self,
        mut command: Command,
        args: &[&str],
        input: Option<Vec<u8>>,
        cbor: bool,
    ) -> Value {
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = Process(
            command
                .args(args)
                .spawn()
                .expect("actual native coordinator"),
        );
        let writer = input.map(|bytes| {
            let mut stdin = child.0.stdin.take().expect("request stdin");
            std::thread::spawn(move || {
                use std::io::Write as _;
                stdin.write_all(&bytes).expect("complete request input");
            })
        });
        let stdout = child.0.stdout.take().expect("stdout");
        let stderr = child.0.stderr.take().expect("stderr");
        let output = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            stdout
                .take(8 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .expect("bounded reply");
            bytes
        });
        let errors = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            stderr
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .expect("bounded errors");
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(40);
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("coordinator status") {
                break status;
            }
            assert!(Instant::now() < deadline, "native coordinator deadline");
            std::thread::sleep(Duration::from_millis(10));
        };
        let bytes = output.join().expect("output reader");
        let errors = errors.join().expect("error reader");
        if let Some(writer) = writer {
            writer.join().expect("request writer");
        }
        assert!(
            status.success(),
            "fixture failed: {}",
            String::from_utf8_lossy(&errors)
        );
        let result: Result<Value, String> = if cbor {
            ciborium::de::from_reader(bytes.as_slice()).map_err(|e| e.to_string())
        } else {
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
        };
        result.unwrap_or_else(|why| {
            panic!(
                "invalid native reply {why}: {} / {}",
                String::from_utf8_lossy(&bytes),
                String::from_utf8_lossy(&errors)
            )
        })
    }

    fn observe(&self, path: &str) -> Value {
        self.raw(true, &["code", "--file-observe", path])
    }

    fn refuses(&self, path: &str, reason: &str) {
        let reply = self.observe(path);
        assert_eq!(reply["ok"], false, "accepted {path}: {reply}");
        assert!(
            reply["error"]
                .as_str()
                .expect("named refusal")
                .contains(reason),
            "wrong refusal for {path}: {reply}"
        );
    }

    fn domain(&self) -> PathBuf {
        self.root.join("var-tmp/.casper-files-v1-0")
    }
}

#[test]
fn actual_native_worker_snapshots_complete_bytes_and_revision_in_both_backends() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        let path = fixture.root.join("work/source.txt");
        let original = format!("HEAD\r\n{}\r\nTAIL λ\n", "x".repeat(400 * 1024));
        std::fs::write(&path, original.as_bytes()).expect("large source");
        let first = fixture.observe("source.txt");
        assert_eq!(first["ok"], true, "bubblewrap={backend}: {first}");
        let snapshot = &first["result"][0];
        assert_eq!(snapshot["source_text"], original);
        assert_eq!(snapshot["source_bytes"], original.len());
        assert_eq!(snapshot["revision_contract"], 1);
        assert_eq!(snapshot["coverage"], "snapshot");
        let second = fixture.observe(path.to_str().expect("absolute source"));
        assert_eq!(second["result"][0]["path_key"], snapshot["path_key"]);
        assert_eq!(
            second["result"][0]["revision"], snapshot["revision"],
            "read atime changed revision"
        );
        std::os::unix::fs::symlink(".", fixture.root.join("work/alias"))
            .expect("parent-directory alias");
        let alias = fixture.observe("alias/source.txt");
        assert_eq!(alias["ok"], true, "parent alias refused: {alias}");
        assert_eq!(alias["result"][0]["path_key"], snapshot["path_key"]);
        assert_eq!(alias["result"][0]["revision"], snapshot["revision"]);
        let changed = original.replacen("HEAD", "EDIT", 1);
        let original_mtime = std::fs::metadata(&path)
            .expect("source metadata")
            .modified()
            .expect("mtime");
        std::fs::write(&path, changed.as_bytes()).expect("same-size change");
        std::fs::File::open(&path)
            .expect("source")
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .expect("restore mtime");
        let third = fixture.observe("source.txt");
        assert_eq!(third["ok"], true, "{third}");
        assert_eq!(third["result"][0]["path_key"], snapshot["path_key"]);
        assert_ne!(third["result"][0]["revision"], snapshot["revision"]);
        assert_eq!(
            std::fs::read(&path).expect("source unchanged"),
            changed.as_bytes()
        );
        std::fs::write(fixture.root.join("work/replacement"), changed.as_bytes())
            .expect("replacement inode");
        std::fs::rename(fixture.root.join("work/replacement"), &path)
            .expect("external inode replacement");
        let replaced = fixture.observe("alias/source.txt");
        assert_eq!(replaced["ok"], true, "replacement snapshot: {replaced}");
        assert_eq!(replaced["result"][0]["path_key"], snapshot["path_key"]);
        assert_ne!(
            replaced["result"][0]["revision"],
            third["result"][0]["revision"]
        );
        assert_eq!(replaced["result"][0]["source_text"], changed);
        assert_eq!(
            std::fs::read_dir(fixture.domain())
                .expect("fixed isolated registry")
                .count(),
            1
        );
    }
}

#[test]
fn actual_native_worker_refuses_unsafe_sources_without_mutation_or_missing_path_leases() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        fixture.refuses("missing", "source binding");
        assert!(
            !fixture.domain().exists(),
            "missing reads provisioned a coordination domain"
        );
        for (name, bytes, reason) in [
            ("invalid", vec![0xff], "UTF-8 without NUL"),
            ("nul", b"before\0after".to_vec(), "UTF-8 without NUL"),
            ("limit", vec![b'x'; 1024 * 1024], "smaller than 1 MiB"),
        ] {
            std::fs::write(fixture.root.join("work").join(name), &bytes).expect("invalid source");
            fixture.refuses(name, reason);
            assert_eq!(
                std::fs::read(fixture.root.join("work").join(name)).expect("unchanged source"),
                bytes
            );
        }
        std::fs::write(fixture.root.join("work/plain"), b"data").expect("source");
        std::os::unix::fs::symlink("plain", fixture.root.join("work/link")).expect("leaf link");
        fixture.refuses("link", "single-linked regular file without a leaf symlink");
        std::fs::hard_link(
            fixture.root.join("work/plain"),
            fixture.root.join("work/hard"),
        )
        .expect("hardlink");
        fixture.refuses("hard", "single-linked regular file without a leaf symlink");
        let secret = fixture.root.join("home/.ssh/id");
        fixture.refuses(secret.to_str().expect("secret path"), "source binding");
        assert_eq!(
            std::fs::read(secret).expect("fixture credential unchanged"),
            b"FIXTURE_SECRET"
        );
    }
}

#[test]
fn actual_native_worker_refuses_uncontained_calls_and_unsafe_fixed_registry() {
    let fixture = Fixture::new(false);
    std::fs::write(fixture.root.join("work/source"), b"data").expect("source");
    assert_eq!(
        fixture.raw(false, &["code", "--file-observe", "source"])["ok"],
        false
    );
    assert!(!fixture.domain().exists());
    std::fs::create_dir(fixture.domain()).expect("unsafe preexisting registry");
    std::fs::set_permissions(fixture.domain(), std::fs::Permissions::from_mode(0o755))
        .expect("unsafe mode");
    fixture.refuses("source", "registry provisioning");
    std::fs::remove_dir(fixture.domain()).expect("remove fixture directory");
    std::os::unix::fs::symlink(fixture.root.join("work"), fixture.domain())
        .expect("registry alias");
    fixture.refuses("source", "protected credential path");
    assert_eq!(
        std::fs::read(fixture.root.join("work/source")).expect("unchanged source"),
        b"data"
    );
}

#[test]
fn actual_native_snapshot_preserves_empty_and_worst_case_escaped_source() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        for (name, bytes) in [("empty", Vec::new()), ("escaped", vec![1; 1024 * 1024 - 1])] {
            std::fs::write(fixture.root.join("work").join(name), &bytes).expect("bounded source");
            let reply = fixture.observe(name);
            assert_eq!(reply["ok"], true, "{name}, backend={backend}: {reply}");
            assert_eq!(reply["result"][0]["source_bytes"], bytes.len());
            assert_eq!(
                reply["result"][0]["source_text"]
                    .as_str()
                    .expect("source")
                    .as_bytes(),
                bytes
            );
        }
    }
}

#[test]
fn ordinary_and_native_grants_cannot_expose_the_fixed_registry_before_creation() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        std::fs::write(fixture.root.join("work/source"), b"data").expect("source");
        for flag in ["--source-text", "--file-observe"] {
            let mut command = fixture.command(true);
            command.env("CASPER_JAIL", r#"{"write":["/var/tmp"]}"#);
            let reply = fixture.run(command, &["code", flag, "source"]);
            assert_eq!(reply["ok"], false, "broad grant exposed registry: {reply}");
            assert!(
                reply["error"]
                    .as_str()
                    .expect("refusal")
                    .contains("protected"),
                "{reply}"
            );
            assert!(
                !fixture.domain().exists(),
                "unsafe grant provisioned domain"
            );
        }
    }
}
