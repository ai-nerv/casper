#[cfg(test)]
mod cases {
    #![allow(clippy::expect_used)]

    use super::super::*;
    use crate::files::locking::{self, Registry};
    use crate::scratch::Scratch;
    use rustix::fs::{self, Mode, OFlags};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::AtomicBool;

    struct Fixture {
        registry: Registry,
        binding: Binding,
        root: Scratch,
    }

    fn directory(path: &Path) -> OwnedFd {
        fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("directory")
    }

    fn binding(root: &Path) -> Binding {
        Binding::from_parent(directory(&root.join("work")), std::ffi::OsStr::new("file"))
            .expect("binding")
    }

    impl Fixture {
        fn new() -> Self {
            let root = Scratch::new("lease-handoff", "control");
            for name in ["registry", "work"] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(root.join(name))
                    .expect("private directory");
            }
            let registry =
                Registry::from_private(directory(&root.join("registry"))).expect("registry");
            let binding = binding(&root);
            Self {
                registry,
                binding,
                root,
            }
        }

        fn free(&self) {
            drop(
                self.registry
                    .delegate(
                        self.binding.identity(),
                        locking::MAX_WAIT,
                        &AtomicBool::new(false),
                    )
                    .expect("lease must be released"),
            );
        }

        fn held(&self) {
            assert!(
                matches!(self.registry.delegate(self.binding.identity(), Duration::ZERO,
                &AtomicBool::new(false)), Err(error) if error.kind() == ErrorKind::TimedOut)
            );
        }

        fn ready(&self, marker: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.root.join(marker).exists() {
                assert!(
                    Instant::now() < deadline,
                    "missing handoff barrier: {marker}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn release(&self, marker: &str) {
            std::fs::write(self.root.join(marker), b"").expect("release barrier");
        }

        fn process(&self, role: &str, socket: OwnedFd) -> Process {
            Process(
                probe_command(role)
                    .env("CASPER_HANDOFF_ROOT", self.root.as_os_str())
                    .stdin(Stdio::from(socket))
                    .spawn()
                    .expect("handoff process"),
            )
        }
    }

    struct Process(Child);

    impl Process {
        fn stop(&mut self) {
            self.0.kill().expect("kill coordinator or worker");
            let _ = self.0.wait().expect("reap child");
        }

        fn finish(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = self.0.try_wait().expect("child status") {
                    assert!(status.success(), "handoff child failed: {status}");
                    return;
                }
                assert!(Instant::now() < deadline, "handoff child timeout");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn child_probes_leave_only_the_parent_completion_record_on_stdout() {
        if std::env::var("CASPER_HANDOFF_RECORD_PROBE").as_deref() == Ok("1") {
            let status = probe_command("fds")
                .env("CASPER_HANDOFF_FORBIDDEN", "no-matching-file-identity")
                .status()
                .expect("descriptor probe");
            assert!(status.success(), "descriptor probe failed");
            return;
        }
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "files::control::tests::cases::child_probes_leave_only_the_parent_completion_record_on_stdout", "--nocapture"])
            .env("CASPER_HANDOFF_RECORD_PROBE", "1")
            .output().expect("parent record probe");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.matches("running 1 test").count(),
            1,
            "nested probe output leaked: {stdout}"
        );
        assert!(stdout.contains("test files::control::tests::cases::child_probes_leave_only_the_parent_completion_record_on_stdout ... ok"), "{stdout}");
        assert!(
            !stdout.contains("test files::control::tests::cases::probe ..."),
            "{stdout}"
        );
    }

    fn probe_command(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "files::control::tests::cases::probe",
                "--nocapture",
            ])
            .env("CASPER_HANDOFF_PROBE", role)
            .stdout(Stdio::null());
        command
    }

    fn wait_for(root: &Path, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.join(marker).exists() {
            assert!(
                Instant::now() < deadline,
                "missing worker barrier: {marker}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn probe() {
        let Ok(role) = std::env::var("CASPER_HANDOFF_PROBE") else {
            return;
        };
        if role == "closed-channels" {
            closed_channel_assertions();
            return;
        }
        if role == "fds" {
            let forbidden = std::env::var("CASPER_HANDOFF_FORBIDDEN").expect("forbidden inode");
            for entry in std::fs::read_dir("/proc/self/fd").expect("descriptors") {
                let entry = entry.expect("descriptor");
                if let Ok(metadata) = std::fs::metadata(entry.path()) {
                    let id = format!("{}:{}", metadata.dev(), metadata.ino());
                    assert!(
                        !forbidden.split(',').any(|value| value == id),
                        "handoff descriptor leaked"
                    );
                }
            }
            return;
        }
        let root = PathBuf::from(std::env::var_os("CASPER_HANDOFF_ROOT").expect("root"));
        let stdin = std::io::stdin();
        if role == "broker" {
            let key = receive_binding(stdin.as_fd()).expect("private worker key");
            let registry =
                Registry::from_private(directory(&root.join("registry"))).expect("registry");
            let lease = registry
                .delegate(&key, locking::MAX_WAIT, &AtomicBool::new(false))
                .expect("delegated lease");
            lease.handoff(stdin.as_fd()).expect("queue rights");
            std::fs::write(root.join("sent"), b"").expect("sent barrier");
            wait_for(&root, "broker-release");
            return;
        }
        assert_eq!(role, "worker");
        let binding = binding(&root);
        send_binding(stdin.as_fd(), &binding).expect("binding request");
        std::fs::write(root.join("requested"), b"").expect("request barrier");
        wait_for(&root, "consume");
        let _lease =
            receive_lease(stdin.as_fd(), &binding).expect("received lease after sender death");
        std::fs::write(root.join("held"), b"").expect("held barrier");
        wait_for(&root, "worker-release");
    }

    #[test]
    fn queued_and_received_rights_retain_the_lease_after_sender_death() {
        let fixture = Fixture::new();
        let (sender, receiver) = pair().expect("private channel");
        let mut worker = fixture.process("worker", receiver);
        fixture.ready("requested");
        let mut broker = fixture.process("broker", sender);
        fixture.ready("sent");
        broker.stop();
        fixture.held();
        fixture.release("consume");
        fixture.ready("held");
        fixture.held();
        fixture.release("worker-release");
        worker.finish();
        fixture.free();
    }

    #[test]
    fn killing_a_receiver_releases_unconsumed_rights_without_a_second_sender() {
        let fixture = Fixture::new();
        let (sender, receiver) = pair().expect("private channel");
        let mut worker = fixture.process("worker", receiver);
        fixture.ready("requested");
        let mut broker = fixture.process("broker", sender);
        fixture.ready("sent");
        broker.stop();
        fixture.held();
        worker.stop();
        fixture.free();
    }

    #[test]
    fn killing_the_worker_releases_received_rights_after_coordinator_death() {
        let fixture = Fixture::new();
        let (sender, receiver) = pair().expect("private channel");
        let mut worker = fixture.process("worker", receiver);
        fixture.ready("requested");
        let mut broker = fixture.process("broker", sender);
        fixture.ready("sent");
        fixture.release("consume");
        fixture.ready("held");
        broker.stop();
        fixture.held();
        worker.stop();
        fixture.free();
    }

    #[test]
    fn privacy_drift_before_handoff_refuses_without_queuing_a_descriptor() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let (sender, receiver) = pair().expect("private channel");
        let lease = fixture
            .registry
            .lease(&fixture.binding, locking::MAX_WAIT, &AtomicBool::new(false))
            .expect("lease");
        std::fs::set_permissions(
            fixture.root.join("registry"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("privacy drift");
        assert!(lease.handoff(sender.as_fd()).is_err());
        assert!(
            matches!(receive(receiver.as_fd(), Instant::now() + Duration::from_millis(30)),
            Err(error) if error.kind() == ErrorKind::TimedOut)
        );
        drop(lease);
        std::fs::set_permissions(
            fixture.root.join("registry"),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("restore private fixture");
        fixture.free();
    }

    #[test]
    fn received_lock_and_private_channel_descriptors_close_on_exec() {
        let fixture = Fixture::new();
        let (sender, receiver) = pair().expect("private channel");
        let lease = fixture
            .registry
            .delegate(
                fixture.binding.identity(),
                locking::MAX_WAIT,
                &AtomicBool::new(false),
            )
            .expect("lease");
        lease.handoff(sender.as_fd()).expect("handoff");
        drop(lease);
        let received = receive_lease(receiver.as_fd(), &fixture.binding).expect("receive");
        fixture.held();
        let mut forbidden = Vec::new();
        for fd in [&received._file, &sender, &receiver] {
            assert!(
                rustix::io::fcntl_getfd(fd)
                    .expect("fd flags")
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            let stat = fs::fstat(fd).expect("fd identity");
            forbidden.push(format!("{}:{}", stat.st_dev, stat.st_ino));
        }
        let mut child = Process(
            probe_command("fds")
                .env("CASPER_HANDOFF_FORBIDDEN", forbidden.join(","))
                .spawn()
                .expect("exec child"),
        );
        child.finish();
        drop(received);
        fixture.free();
    }

    fn raw(fd: BorrowedFd<'_>, body: &[u8], rights: &[BorrowedFd<'_>]) {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(8))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        if !rights.is_empty() {
            assert!(ancillary.push(SendAncillaryMessage::ScmRights(rights)));
        }
        assert_eq!(
            net::sendmsg(
                fd,
                &[IoSlice::new(body)],
                &mut ancillary,
                SendFlags::NOSIGNAL
            )
            .expect("raw fault packet"),
            body.len()
        );
    }

    fn lease_body(key: &Key, fd: BorrowedFd<'_>) -> Vec<u8> {
        let stat = fs::fstat(fd).expect("fd identity");
        let mut body = LEASE.to_vec();
        body.extend_from_slice(key.as_str().as_bytes());
        body.extend_from_slice(&stat.st_dev.to_le_bytes());
        body.extend_from_slice(&stat.st_ino.to_le_bytes());
        body
    }

    #[test]
    fn malformed_truncated_duplicate_and_wrong_binding_packets_drop_all_rights() {
        for fault in [
            "version",
            "binding",
            "identity",
            "short",
            "long",
            "duplicate",
            "truncated",
            "no-fd",
            "key-fd",
        ] {
            let fixture = Fixture::new();
            let (sender, receiver) = pair().expect("channel");
            let lease = fixture
                .registry
                .delegate(
                    fixture.binding.identity(),
                    locking::MAX_WAIT,
                    &AtomicBool::new(false),
                )
                .expect("lease");
            let (bootstrap_sender, bootstrap_receiver) = pair().expect("valid initial handoff");
            lease
                .handoff(bootstrap_sender.as_fd())
                .expect("initial handoff");
            let transferred = receive_lease(bootstrap_receiver.as_fd(), &fixture.binding)
                .expect("actual locked description");
            let fd = transferred._file.as_fd();
            let mut body = lease_body(fixture.binding.identity(), fd);
            match fault {
                "version" => *body.first_mut().expect("magic") = b'?',
                "binding" => {
                    body = lease_body(&Key::from_wire(&"0".repeat(64)).expect("other key"), fd);
                }
                "identity" => *body.last_mut().expect("identity") ^= 1,
                "short" => {
                    body.pop();
                }
                "long" => {
                    body.resize(MAX_PACKET + 1, b'x');
                }
                "key-fd" => {
                    body = BINDING.to_vec();
                    body.extend_from_slice(fixture.binding.key().as_bytes());
                }
                _ => {}
            }
            let count = match fault {
                "no-fd" => 0,
                "duplicate" => 2,
                "truncated" => 8,
                _ => 1,
            };
            raw(sender.as_fd(), &body, &vec![fd; count]);
            drop(lease);
            drop(transferred);
            let rejected = if fault == "key-fd" {
                receive_binding(receiver.as_fd()).is_err()
            } else {
                receive_lease(receiver.as_fd(), &fixture.binding).is_err()
            };
            assert!(rejected, "accepted malformed {fault}");
            fixture.free();
        }
    }

    #[test]
    fn stream_sockets_bad_keys_missing_descriptors_and_closed_channels_refuse() {
        let mut child = Process(
            probe_command("closed-channels")
                .spawn()
                .expect("isolated channel-close probe"),
        );
        child.finish();
    }

    fn closed_channel_assertions() {
        let fixture = Fixture::new();
        for key in [
            "",
            "../escape",
            &"A".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            assert!(Key::from_wire(key).is_err());
        }
        let (stream, _other) = std::os::unix::net::UnixStream::pair().expect("stream");
        assert!(send_binding(stream.as_fd(), &fixture.binding).is_err());
        let (sender, receiver) = pair().expect("channel");
        send_binding(sender.as_fd(), &fixture.binding).expect("request");
        let key = receive_binding(receiver.as_fd()).expect("key");
        assert_eq!(key.as_str(), fixture.binding.key());
        drop(sender);
        assert!(
            matches!(receive_binding(receiver.as_fd()), Err(error) if error.kind() == ErrorKind::UnexpectedEof)
        );
        let (sender, receiver) = pair().expect("closed receiver");
        drop(receiver);
        assert!(send_binding(sender.as_fd(), &fixture.binding).is_err());
        let (sender, receiver) = pair().expect("aliased receiver");
        let alias = receiver.try_clone().expect("another open-file reference");
        drop(receiver);
        send_binding(sender.as_fd(), &fixture.binding).expect("peer still lives through alias");
        drop(alias);
        assert!(send_binding(sender.as_fd(), &fixture.binding).is_err());
    }

    #[test]
    fn idle_control_waits_are_bounded_without_creating_a_registry_entry() {
        let fixture = Fixture::new();
        let (_sender, receiver) = pair().expect("channel");
        let started = Instant::now();
        assert!(
            matches!(receive(receiver.as_fd(), started + Duration::from_millis(30)),
            Err(error) if error.kind() == ErrorKind::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            std::fs::read_dir(fixture.root.join("registry"))
                .expect("registry")
                .count(),
            0
        );
    }
}
