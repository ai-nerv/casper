#![allow(clippy::unwrap_used)]

use casper::scratch::Scratch;
use casper::workflows::Failure;
use casper::workflows::protocol::{Operation, Payload, Request, Response};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const HEADER: &str = "---\nname: review\ndescription: Review changes\n---\n";
const SOURCE: &str =
    "---\nname: review\ndescription: Review changes\n---\nLiteral $HOME `touch forbidden`\n";

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(
            rustix::process::Pid::from_child(&self.0),
            rustix::process::Signal::KILL,
        );
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(fixture: &Scratch, request: &Request, bwrap: bool, args: &[&str]) -> Vec<u8> {
    let output = fixture.join("output");
    let error = fixture.join("error");
    let mut command = Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .arg("workflows")
        .args(args)
        .env_clear()
        .env("HOME", fixture.join("home"))
        .env("XDG_CONFIG_HOME", fixture.join("config"))
        .env("XDG_DATA_HOME", fixture.join("data"))
        .env("XDG_CACHE_HOME", fixture.join("cache"))
        .env("XDG_RUNTIME_DIR", fixture.join("runtime"))
        .env(
            "PATH",
            if bwrap {
                std::env::var_os("PATH").unwrap()
            } else {
                fixture.join("empty-bin").into_os_string()
            },
        )
        .current_dir(fixture)
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(&output).unwrap())
        .stderr(std::fs::File::create(&error).unwrap());
    casper::tied::running(&mut command, Vec::new());
    let mut child = Owned(command.spawn().unwrap());
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        if let Some(status) = waitid(
            WaitId::Pid(rustix::process::Pid::from_child(&child.0)),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .unwrap()
        {
            assert_eq!(
                status.exit_status(),
                Some(0),
                "{}",
                std::fs::read_to_string(&error).unwrap()
            );
            break;
        }
        assert!(Instant::now() < deadline, "workflow CLI timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(child);
    let mut bytes = Vec::new();
    std::fs::File::open(output)
        .unwrap()
        .take(8 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() < 8 * 1024 * 1024);
    assert!(std::fs::metadata(error).unwrap().len() <= 4096);
    bytes
}

fn invoke(fixture: &Scratch, request: &Request, bwrap: bool) -> Result<Payload, Failure> {
    let bytes = run(fixture, request, bwrap, &["--json"]);
    let reply: casper::wire::Reply = serde_json::from_slice(&bytes).unwrap();
    assert!(reply.ok, "{reply:?}");
    assert_eq!(reply.family, casper::wire::FAMILY);
    assert_eq!(reply.n, 1);
    assert_eq!(reply.result.len(), 1);
    let response: Response =
        serde_json::from_value(reply.result.into_iter().next().unwrap()).unwrap();
    response.validate(request).unwrap();
    response.result
}

fn exercise(bwrap: bool) {
    let fixture = Scratch::new("workflow", "cli");
    let root = fixture.join("config").join("magi").join("skills");
    std::fs::create_dir_all(root.join("review")).unwrap();
    std::fs::write(root.join("review/SKILL.md"), SOURCE).unwrap();
    std::fs::write(root.parent().unwrap().join("init.lua"), "untouched").unwrap();
    let mut request = Request {
        version: 1,
        root: root.clone(),
        operation: Operation::Catalog {},
    };
    let Payload::Catalog(catalog) = invoke(&fixture, &request, bwrap).unwrap() else {
        panic!("catalog expected")
    };
    assert_eq!(catalog.entries.len(), 1);
    let candidate = catalog.entries.first().unwrap();
    assert_eq!(candidate.name, "review");
    assert_eq!(candidate.metadata.as_deref(), Ok(HEADER));
    request.operation = Operation::Load {
        name: "review".into(),
    };
    let Payload::Source(source) = invoke(&fixture, &request, bwrap).unwrap() else {
        panic!("source expected")
    };
    assert_eq!(source.text, SOURCE);
    assert_eq!(source, casper::workflows::load(&root, "review").unwrap());
    std::fs::remove_file(root.join("review/SKILL.md")).unwrap();
    assert_eq!(invoke(&fixture, &request, bwrap), Err(Failure::NotFound));
    assert_eq!(
        std::fs::read_to_string(root.parent().unwrap().join("init.lua")).unwrap(),
        "untouched"
    );
    assert!(!fixture.join("forbidden").exists());
}

#[test]
fn real_cli_catalog_and_source_through_bubblewrap() {
    assert!(
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .any(|path| path.join("bwrap").is_file()),
        "bwrap prerequisite"
    );
    exercise(true);
}

#[test]
fn real_cli_catalog_and_source_through_landlock() {
    exercise(false);
}

#[test]
fn absent_catalog_is_empty_but_alias_and_worker_bypass_fail() {
    let fixture = Scratch::new("workflow", "refusal");
    let mut request = Request {
        version: 1,
        root: fixture.join("missing"),
        operation: Operation::Catalog {},
    };
    let Payload::Catalog(catalog) = invoke(&fixture, &request, false).unwrap() else {
        panic!("catalog expected")
    };
    assert!(catalog.entries.is_empty());
    std::fs::create_dir(fixture.join("root")).unwrap();
    std::os::unix::fs::symlink(fixture.join("root"), fixture.join("alias")).unwrap();
    request.root = fixture.join("alias");
    assert!(invoke(&fixture, &request, false).is_err());
    request.root = fixture.join("root");
    for (args, expected) in [
        (vec!["--worker"], Failure::Worker),
        (vec!["--worker", "ignored"], Failure::Protocol),
    ] {
        let bytes = run(&fixture, &request, false, &args);
        let response: Response = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.result, Err(expected));
    }
}
