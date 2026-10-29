use super::{Fixture, Process, TOOLS, install, mutation, read, result};
use casper::files::locking::{Binding, Registry};
use rustix::fs::{Mode, OFlags};
use serde_json::{Value, json};
use std::io::{BufRead as _, Read, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PROBE: &str = "admission::parallel::dispatch_probe";

fn capture(input: impl Read + Send + 'static, limit: u64) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        input
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .expect("bounded pipe");
        assert!(
            bytes.len() as u64 <= limit,
            "fixture pipe exceeded its bound"
        );
        bytes
    })
}

pub(super) fn completed(mut child: Process) -> Vec<u8> {
    let stdout = capture(child.0.stdout.take().expect("reply pipe"), 8 * 1024 * 1024);
    let stderr = capture(child.0.stderr.take().expect("error pipe"), 64 * 1024);
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("actual process status") {
            break status;
        }
        assert!(Instant::now() < deadline, "actual process deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout.join().expect("reply reader");
    let stderr = stderr.join().expect("error reader");
    assert!(status.success(), "{}", String::from_utf8_lossy(&stderr));
    stdout
}

fn finish(child: Process) -> Value {
    let stdout = completed(child);
    serde_json::from_slice(&stdout).unwrap_or_else(|error| {
        panic!(
            "invalid actual reply: {error}: {}",
            String::from_utf8_lossy(&stdout)
        )
    })
}

pub(super) struct Client {
    binary: PathBuf,
    launcher: Option<PathBuf>,
    socket: Option<PathBuf>,
    _server: Option<Process>,
}

impl Client {
    pub(super) fn new(binary: PathBuf, socket: bool) -> Self {
        Self::with_fault(binary, socket, None)
    }

    pub(super) fn with_fault(binary: PathBuf, socket: bool, launcher: Option<PathBuf>) -> Self {
        let mut client = Self {
            binary,
            launcher,
            socket: None,
            _server: None,
        };
        if !socket {
            return client;
        }
        let path = std::env::temp_dir().join(if client.launcher.is_some() {
            "casper-0/fault"
        } else {
            "casper-0/s"
        });
        let mut server = Process(
            client
                .command()
                .args(["serve", "--at"])
                .arg(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("actual shared daemon"),
        );
        server
            .0
            .stdin
            .take()
            .expect("daemon key input")
            .write_all(format!("{}\n", "5e".repeat(32)).as_bytes())
            .expect("daemon key");
        let stdout = server.0.stdout.take().expect("daemon announcement");
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufReader::new(stdout)
                .take(1024)
                .read_line(&mut line)
                .expect("bounded daemon announcement");
            let _ = send.send(line);
        });
        let event: Value = serde_json::from_str(
            &receive
                .recv_timeout(Duration::from_secs(5))
                .expect("daemon announcement deadline"),
        )
        .expect("actual daemon event");
        assert_eq!(event["event"], "listening");
        assert_eq!(event["file_admission"], 1);
        assert_eq!(event["at"], path.to_string_lossy().as_ref());
        client.socket = Some(path);
        client._server = Some(server);
        client
    }

    fn command(&self) -> Command {
        if let Some(launcher) = &self.launcher {
            let mut command = Command::new(launcher);
            command.arg(&self.binary);
            command
        } else {
            Command::new(&self.binary)
        }
    }

    pub(super) fn call(&self, request: Value) -> Value {
        if let Some(path) = &self.socket {
            let mut stream = UnixStream::connect(path).expect("same-daemon client connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("reply deadline");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("send deadline");
            let call = json!({"call":"run","key":"5e".repeat(32),"args":[request]});
            casper::framing::write_frame(
                &mut stream,
                &serde_json::to_vec(&call).expect("keyed frame"),
            )
            .expect("actual request");
            return casper::framing::decode(
                &casper::framing::read_frame(&mut stream).expect("actual daemon reply"),
            )
            .expect("typed daemon reply");
        }
        let mut child = Process(
            self.command()
                .args(["run", "--json"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("actual command coordinator"),
        );
        let mut input = child.0.stdin.take().expect("call input");
        let request = serde_json::to_vec(&request).expect("native call");
        let writer = std::thread::spawn(move || input.write_all(&request));
        let reply = finish(child);
        writer
            .join()
            .expect("input writer")
            .expect("complete input");
        reply
    }
}

fn directory(path: &Path) -> std::os::fd::OwnedFd {
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .expect("private directory")
}

fn actual_worker_is_running(binary: &Path, work: &Path) -> bool {
    std::fs::read_dir("/proc")
        .expect("process inventory")
        .filter_map(Result::ok)
        .any(|entry| {
            let proc = entry.path();
            std::fs::read_link(proc.join("exe")).is_ok_and(|exe| exe == binary)
                && std::fs::read_link(proc.join("cwd")).is_ok_and(|cwd| cwd == work)
                && std::fs::read(proc.join("cmdline")).is_ok_and(|args| {
                    args.split(|byte| *byte == 0)
                        .any(|arg| arg == b"--file-worker")
                })
        })
}

fn actual_process(
    binary: &Path,
    work: &Path,
    argument: &[u8],
    binding: Option<&std::fs::Metadata>,
) -> Option<std::os::fd::OwnedFd> {
    std::fs::read_dir("/proc")
        .expect("actual process inventory")
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            if std::fs::read_link(path.join("exe")).ok()?.as_path() != binary
                || std::fs::read_link(path.join("cwd")).ok()?.as_path() != work
                || !std::fs::read(path.join("cmdline"))
                    .ok()?
                    .split(|byte| *byte == 0)
                    .any(|arg| arg == argument)
            {
                return None;
            }
            if let Some(binding) = binding {
                let opened = std::fs::read_dir(path.join("fd"))
                    .ok()?
                    .filter_map(Result::ok)
                    .any(|fd| {
                        let matches = std::fs::metadata(fd.path()).is_ok_and(|metadata| {
                            (metadata.dev(), metadata.ino()) == (binding.dev(), binding.ino())
                        });
                        if !matches {
                            return false;
                        }
                        std::fs::read_to_string(path.join("fdinfo").join(fd.file_name())).is_ok_and(
                            |info| {
                                info.lines()
                                    .filter_map(|line| line.strip_prefix("flags:"))
                                    .filter_map(|flags| u64::from_str_radix(flags.trim(), 8).ok())
                                    .any(|flags| {
                                        flags & 3 == 2 && flags & OFlags::PATH.bits() as u64 == 0
                                    })
                            },
                        )
                    });
                if !opened {
                    return None;
                }
            }
            let pid = entry
                .file_name()
                .to_str()?
                .parse()
                .ok()
                .and_then(rustix::process::Pid::from_raw)?;
            rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok()
        })
}

fn waiting_coordinator(
    client: &Client,
    work: &Path,
    lock: &std::fs::Metadata,
) -> Option<std::os::fd::OwnedFd> {
    actual_process(
        &client.binary,
        work,
        if client.socket.is_some() {
            b"serve"
        } else {
            b"run"
        },
        Some(lock),
    )
}

#[test]
fn cancellation_probe() {
    let Some(binary) = std::env::var_os("CASPER_PARALLEL_BINARY") else {
        return;
    };
    let client = Client::new(
        PathBuf::from(binary),
        std::env::var("CASPER_PARALLEL_DOOR").expect("actual door") == "library",
    );
    let kill_coordinator =
        std::env::var("CASPER_PARALLEL_CANCEL").expect("cancelled owner") == "coordinator";
    let work = std::env::current_dir().expect("fixture source directory");
    let observed = client.call(read("first"));
    let binding = Binding::from_parent(directory(&work), std::ffi::OsStr::new("first"))
        .expect("source binding");
    assert_eq!(
        result(&observed)["observations"][0]["path_key"],
        binding.key()
    );
    let registry = Registry::from_private(directory(Path::new("/var/tmp/.casper-files-v1-0")))
        .expect("actual registry");
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let lease = registry
        .lease(&binding, Duration::from_secs(1), &cancelled)
        .expect("external first lease");
    let lock_path = Path::new("/var/tmp/.casper-files-v1-0").join(binding.key());
    let lock = std::fs::metadata(&lock_path).expect("actual held lock identity");
    let before = std::fs::metadata("first").expect("source inode");
    let request = mutation(
        "change",
        json!({"path":"first","old":"old","new":"unsafe"}),
        result(&observed)["observations"][0]["revision"]
            .as_str()
            .expect("actual revision"),
        "cancelled-waiter",
    );
    std::thread::scope(|scope| {
        let pending = scope.spawn(|| client.call(request));
        let deadline = Instant::now() + Duration::from_secs(3);
        let worker = loop {
            if let Some(worker) = actual_process(&client.binary, &work, b"--file-worker", None) {
                break worker;
            }
            assert!(
                !pending.is_finished(),
                "actual call ended before its waiting worker was observed"
            );
            assert!(Instant::now() < deadline, "actual waiting worker deadline");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            !pending.is_finished(),
            "first-key lease did not retain the waiter"
        );
        let coordinator = loop {
            if let Some(coordinator) = waiting_coordinator(&client, &work, &lock) {
                break coordinator;
            }
            assert!(
                !pending.is_finished(),
                "coordinator ended before opening its bound target lock"
            );
            assert!(
                Instant::now() < deadline,
                "actual coordinator binding-fd barrier"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let victim = if kill_coordinator {
            coordinator
        } else {
            worker.try_clone().expect("stable worker identity")
        };
        rustix::process::pidfd_send_signal(&victim, rustix::process::Signal::KILL)
            .expect("kill selected actual process");
        let mut terminal = [rustix::event::PollFd::new(
            &worker,
            rustix::event::PollFlags::IN,
        )];
        assert_eq!(
            rustix::event::poll(
                &mut terminal,
                Some(&rustix::event::Timespec {
                    tv_sec: 3,
                    tv_nsec: 0
                })
            )
            .expect("actual worker termination"),
            1,
            "cancelled waiting worker remained live before external lease release"
        );
        assert_eq!(
            std::fs::read("first").expect("target before lease release"),
            b"first=old\n"
        );
        drop(lease);
        let completion = pending.join();
        if kill_coordinator {
            assert!(
                completion.is_err(),
                "killed coordinator falsely delivered a successful reply"
            );
        } else {
            let reply = completion.expect("live coordinator must classify failed handoff");
            let ran = result(&reply);
            assert_eq!(ran["failed"], true, "{reply}");
            assert_eq!(ran["mutation"]["id"], "cancelled-waiter", "{reply}");
            assert!(
                matches!(
                    ran["mutation"]["outcome"].as_str(),
                    Some("refused" | "unknown")
                ),
                "{reply}"
            );
            assert!(ran.get("observations").is_none(), "{reply}");
        }
    });
    assert_eq!(
        std::fs::read("first").expect("no later commit"),
        b"first=old\n"
    );
    assert_eq!(
        std::fs::metadata("first").expect("same target inode").ino(),
        before.ino()
    );
    assert!(
        registry
            .lease(&binding, Duration::from_secs(1), &cancelled)
            .is_ok(),
        "cancelled call retained a lease"
    );
    let after = std::fs::metadata(&lock_path).expect("retained registry lock");
    assert_eq!((after.dev(), after.ino()), (lock.dev(), lock.ino()));
    for entry in std::fs::read_dir(&work).expect("stage inventory") {
        assert_eq!(entry.expect("source entry").file_name(), "first");
    }
}

#[test]
fn actual_waiting_native_workers_cannot_commit_after_worker_or_coordinator_death_on_both_doors_and_backends()
 {
    for backend in [false, true] {
        for door in ["command", "library"] {
            for owner in ["worker", "coordinator"] {
                let fixture = Fixture::new(backend);
                install(&fixture, TOOLS);
                std::fs::write(fixture.root.join("work/first"), "first=old\n").expect("source");
                let mut command = fixture.command_program(
                    true,
                    None,
                    &std::env::current_exe().expect("probe executable"),
                );
                command
                    .args([
                        "--exact",
                        "admission::parallel::cancellation_probe",
                        "--nocapture",
                    ])
                    .env("CASPER_PARALLEL_BINARY", &fixture.binary)
                    .env("CASPER_PARALLEL_DOOR", door)
                    .env("CASPER_PARALLEL_CANCEL", owner);
                let output =
                    completed(Process(command.spawn().expect("actual cancellation probe")));
                assert!(
                    String::from_utf8_lossy(&output)
                        .contains("test admission::parallel::cancellation_probe ... ok")
                );
                assert_eq!(
                    std::fs::read(fixture.root.join("work/first")).expect("intact target"),
                    b"first=old\n"
                );
            }
        }
    }
}

#[test]
fn dispatch_probe() {
    let Some(binary) = std::env::var_os("CASPER_PARALLEL_BINARY") else {
        return;
    };
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let socket = std::env::var("CASPER_PARALLEL_DOOR").expect("selected door") == "library";
    let client = Client::new(PathBuf::from(binary), socket);
    let work = std::env::current_dir().expect("fixture work");
    let first = client.call(read("first"));
    let second = client.call(read("second"));
    let binding = Binding::from_parent(directory(&work), std::ffi::OsStr::new("first"))
        .expect("first source binding");
    assert_eq!(result(&first)["observations"][0]["path_key"], binding.key());
    let registry = Registry::from_private(directory(Path::new("/var/tmp/.casper-files-v1-0")))
        .expect("actual private registry");
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let lease = registry
        .lease(&binding, Duration::from_secs(1), &cancelled)
        .expect("held first lease");
    let lock_path = Path::new("/var/tmp/.casper-files-v1-0").join(binding.key());
    let lock = std::fs::metadata(&lock_path).expect("retained lock inode");
    let first_call = mutation(
        "change",
        json!({"path":"first","old":"old","new":"new"}),
        result(&first)["observations"][0]["revision"]
            .as_str()
            .expect("first revision"),
        "blocked-first",
    );
    let second_call = mutation(
        "change",
        json!({"path":"second","old":"old","new":"new"}),
        result(&second)["observations"][0]["revision"]
            .as_str()
            .expect("second revision"),
        "independent-second",
    );
    let committed_first = std::thread::scope(|scope| {
        let (first_send, first_receive) = mpsc::channel();
        let client = &client;
        scope.spawn(move || {
            first_send
                .send(client.call(first_call))
                .expect("first result channel")
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !actual_worker_is_running(&client.binary, &work)
            || waiting_coordinator(client, &work, &lock).is_none()
        {
            assert!(
                matches!(first_receive.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "first call ended before worker barrier"
            );
            assert!(
                Instant::now() < deadline,
                "actual blocked native worker was not observed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let (second_send, second_receive) = mpsc::channel();
        scope.spawn(move || {
            second_send
                .send(client.call(second_call))
                .expect("second result channel")
        });
        let committed = second_receive
            .recv_timeout(Duration::from_secs(3))
            .expect("independent actual worker must finish while first key is held");
        assert_eq!(
            result(&committed)["mutation"]["outcome"],
            "committed",
            "{committed}"
        );
        assert_eq!(result(&committed)["mutation"]["id"], "independent-second");
        assert_eq!(
            result(&committed)["observations"][0]["path_key"],
            result(&second)["observations"][0]["path_key"]
        );
        assert_ne!(
            result(&committed)["observations"][0]["revision"],
            result(&second)["observations"][0]["revision"]
        );
        assert_eq!(
            std::fs::read("second").expect("independent committed file"),
            b"second=new\n"
        );
        assert_eq!(
            std::fs::read("first").expect("blocked target intact"),
            b"first=old\n"
        );
        assert!(
            matches!(first_receive.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "first call completed while its lease was externally held"
        );
        drop(lease);
        first_receive
            .recv_timeout(Duration::from_secs(10))
            .expect("first worker resumes after lease release")
    });
    assert_eq!(
        result(&committed_first)["mutation"]["outcome"],
        "committed",
        "{committed_first}"
    );
    assert_eq!(result(&committed_first)["mutation"]["id"], "blocked-first");
    assert_eq!(
        result(&committed_first)["observations"][0]["path_key"],
        binding.key()
    );
    assert_ne!(
        result(&committed_first)["observations"][0]["revision"],
        result(&first)["observations"][0]["revision"]
    );
    assert_eq!(
        std::fs::read("first").expect("first committed file"),
        b"first=new\n"
    );
    let after = std::fs::metadata(&lock_path).expect("same lock inode");
    assert_eq!((after.dev(), after.ino()), (lock.dev(), lock.ino()));
    assert!(
        registry
            .lease(&binding, Duration::from_secs(1), &cancelled)
            .is_ok()
    );
    for entry in std::fs::read_dir(&work).expect("source directory") {
        assert!(
            !entry
                .expect("source entry")
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".casper-stage-")
        );
    }
}

#[test]
fn actual_independent_file_workers_overlap_on_both_doors_and_backends() {
    for backend in [false, true] {
        for door in ["command", "library"] {
            let fixture = Fixture::new(backend);
            install(&fixture, TOOLS);
            std::fs::write(fixture.root.join("work/first"), "first=old\n").expect("first source");
            std::fs::write(fixture.root.join("work/second"), "second=old\n")
                .expect("second source");
            let mut command = fixture.command_program(
                true,
                None,
                &std::env::current_exe().expect("probe executable"),
            );
            command
                .args(["--exact", PROBE, "--nocapture"])
                .env("CASPER_PARALLEL_BINARY", &fixture.binary)
                .env("CASPER_PARALLEL_DOOR", door);
            let output = completed(Process(command.spawn().expect("actual dispatch probe")));
            assert!(String::from_utf8_lossy(&output).contains(&format!("test {PROBE} ... ok")));
            assert_eq!(
                std::fs::read(fixture.root.join("work/first")).expect("first committed file"),
                b"first=new\n"
            );
            assert_eq!(
                std::fs::read(fixture.root.join("work/second")).expect("second committed file"),
                b"second=new\n"
            );
        }
    }
}
