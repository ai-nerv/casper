#[cfg(test)]
mod cases {
    #![allow(clippy::expect_used)]

    use super::super::*;
    use crate::scratch::Scratch;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};

    struct Fixture {
        registry: Registry,
        root: Scratch,
    }

    fn directory(path: &Path) -> OwnedFd {
        fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("directory descriptor")
    }

    fn binding(path: &Path) -> Binding {
        Binding::from_parent(
            directory(path.parent().expect("parent")),
            path.file_name().expect("leaf"),
        )
        .expect("file binding")
    }

    impl Fixture {
        fn new() -> Self {
            let root = Scratch::new("file-leases", "registry");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("registry"))
                .expect("private registry");
            std::fs::create_dir(root.join("work")).expect("workspace");
            let registry = Registry::from_private(directory(&root.join("registry")))
                .expect("validated registry");
            Self { registry, root }
        }

        fn target(&self, name: &str) -> PathBuf {
            self.root.join("work").join(name)
        }

        fn named(&self, binding: &Binding) -> PathBuf {
            self.root.join("registry").join(binding.key())
        }

        fn start(&self, id: &str, target: &Path) -> Process {
            let mut command = probe_command("hold");
            command
                .env("CASPER_FILE_LEASE_ROOT", self.root.as_os_str())
                .env("CASPER_FILE_LEASE_TARGET", target)
                .env("CASPER_FILE_LEASE_ID", id);
            Process(command.spawn().expect("independent lease process"))
        }

        fn ready(&self, marker: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.root.join(marker).exists() {
                assert!(Instant::now() < deadline, "missing lease barrier: {marker}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn release(&self, id: &str) {
            std::fs::write(self.root.join(format!("{id}.release")), b"").expect("release barrier");
        }
    }

    struct Process(Child);

    impl Process {
        fn stop(&mut self) {
            self.0.kill().expect("kill process");
            let _ = self.0.wait().expect("reap process");
        }

        fn finish(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = self.0.try_wait().expect("child status") {
                    assert!(status.success(), "lease probe failed: {status}");
                    return;
                }
                assert!(Instant::now() < deadline, "lease probe did not finish");
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
        if std::env::var("CASPER_LEASE_RECORD_PROBE").as_deref() == Ok("1") {
            let status = probe_command("fds")
                .env("CASPER_FILE_LEASE_FORBIDDEN", "no-matching-file-identity")
                .status()
                .expect("descriptor probe");
            assert!(status.success(), "descriptor probe failed");
            return;
        }
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "files::locking::tests::cases::child_probes_leave_only_the_parent_completion_record_on_stdout", "--nocapture"])
            .env("CASPER_LEASE_RECORD_PROBE", "1")
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
        assert!(stdout.contains("test files::locking::tests::cases::child_probes_leave_only_the_parent_completion_record_on_stdout ... ok"), "{stdout}");
        assert!(
            !stdout.contains("test files::locking::tests::cases::probe ..."),
            "{stdout}"
        );
    }

    fn probe_command(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "files::locking::tests::cases::probe",
                "--nocapture",
            ])
            .env("CASPER_FILE_LEASE_PROBE", role)
            .stdout(Stdio::null());
        command
    }

    #[test]
    fn probe() {
        let Ok(role) = std::env::var("CASPER_FILE_LEASE_PROBE") else {
            return;
        };
        if role == "fds" {
            let forbidden =
                std::env::var("CASPER_FILE_LEASE_FORBIDDEN").expect("forbidden bindings");
            for entry in std::fs::read_dir("/proc/self/fd").expect("process descriptors") {
                let entry = entry.expect("descriptor");
                if let Ok(metadata) = std::fs::metadata(entry.path()) {
                    let identity = format!("{}:{}", metadata.dev(), metadata.ino());
                    assert!(
                        !forbidden.split(',').any(|bound| bound == identity),
                        "lease descriptor survived exec: {}",
                        entry.path().display()
                    );
                }
            }
            return;
        }
        assert_eq!(role, "hold");
        let root =
            PathBuf::from(std::env::var_os("CASPER_FILE_LEASE_ROOT").expect("registry root"));
        let target = PathBuf::from(std::env::var_os("CASPER_FILE_LEASE_TARGET").expect("target"));
        let id = std::env::var("CASPER_FILE_LEASE_ID").expect("process id");
        let registry = Registry::from_private(directory(&root.join("registry"))).expect("registry");
        let binding = binding(&target);
        let mut notified = false;
        let _lease = registry
            .lease_with(&binding, MAX_WAIT, &AtomicBool::new(false), |stage| {
                if stage == WaitFor::Target && !notified {
                    std::fs::write(root.join(format!("{id}.waiting")), b"").expect("wait barrier");
                    notified = true;
                }
            })
            .expect("lease");
        std::fs::write(root.join(format!("{id}.held")), b"").expect("held barrier");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.join(format!("{id}.release")).exists() {
            assert!(Instant::now() < deadline, "release barrier never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn binding_aliases_and_target_replacements_keep_one_key() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.target("sub")).expect("subdirectory");
        std::os::unix::fs::symlink(fixture.root.join("work"), fixture.root.join("alias"))
            .expect("parent alias");
        let original = binding(&fixture.target("file"));
        let aliases = [
            binding(&fixture.target("sub/../file")),
            binding(&fixture.root.join("alias/file")),
        ];
        for alias in &aliases {
            assert_eq!(alias.key(), original.key());
        }
        std::fs::write(fixture.target("replacement"), b"new inode").expect("replacement");
        std::fs::rename(fixture.target("replacement"), fixture.target("file"))
            .expect("replace target");
        let replaced = binding(&fixture.target("file"));
        assert_eq!(original.key(), replaced.key());
        assert_ne!(original.key(), binding(&fixture.target("other")).key());
        let cancelled = AtomicBool::new(false);
        let _lease = fixture
            .registry
            .lease(&original, Duration::ZERO, &cancelled)
            .expect("lease");
        for alias in aliases.iter().chain(std::iter::once(&replaced)) {
            assert!(matches!(
                fixture.registry.lease(alias, Duration::ZERO, &cancelled),
                Err(error) if error.kind() == ErrorKind::TimedOut
            ));
        }
        for name in ["", ".", "..", "a/b", "a\0b"] {
            assert!(
                Binding::from_parent(directory(&fixture.root.join("work")), OsStr::new(name))
                    .is_err()
            );
        }
        assert!(
            Binding::from_parent(
                directory(&fixture.root.join("work")),
                OsStr::new(&"x".repeat(256))
            )
            .is_err()
        );
    }

    #[test]
    fn distinct_keys_overlap_while_another_process_waits() {
        let fixture = Fixture::new();
        let mut first = fixture.start("first", &fixture.target("file"));
        fixture.ready("first.held");
        let mut waiting = fixture.start("waiting", &fixture.target("file"));
        fixture.ready("waiting.waiting");
        let mut independent = fixture.start("independent", &fixture.target("other"));
        fixture.ready("independent.held");
        assert!(!fixture.root.join("waiting.held").exists());
        fixture.release("independent");
        independent.finish();
        fixture.release("first");
        first.finish();
        fixture.ready("waiting.held");
        fixture.release("waiting");
        waiting.finish();
    }

    #[test]
    fn killed_waiters_and_holders_release_only_their_own_leases() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let mut holder = fixture.start("holder", &fixture.target("file"));
        fixture.ready("holder.held");
        let before = std::fs::metadata(fixture.named(&binding))
            .expect("persistent lock")
            .ino();
        let mut waiter = fixture.start("waiter", &fixture.target("file"));
        fixture.ready("waiter.waiting");
        waiter.stop();
        let cancelled = AtomicBool::new(false);
        assert!(matches!(
            fixture.registry.lease(&binding, Duration::ZERO, &cancelled),
            Err(error) if error.kind() == ErrorKind::TimedOut
        ));
        holder.stop();
        let _lease = fixture
            .registry
            .lease(&binding, Duration::ZERO, &cancelled)
            .expect("released lease");
        assert_eq!(
            before,
            std::fs::metadata(fixture.named(&binding))
                .expect("same lock")
                .ino()
        );
    }

    #[test]
    fn cancellation_and_deadlines_leave_the_lock_inode_in_place() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        let lease = fixture
            .registry
            .lease(&binding, Duration::ZERO, &cancelled)
            .expect("holder");
        let before = std::fs::metadata(fixture.named(&binding))
            .expect("lock")
            .ino();
        let began = Instant::now();
        assert!(matches!(
            fixture.registry.lease(&binding, Duration::from_millis(30), &cancelled),
            Err(error) if error.kind() == ErrorKind::TimedOut
        ));
        assert!(began.elapsed() < Duration::from_secs(2));
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                fixture
                    .registry
                    .lease_with(&binding, MAX_WAIT, &cancelled, |stage| {
                        if stage == WaitFor::Target {
                            let _ = send.try_send(());
                        }
                    })
                    .err()
                    .expect("cancelled waiter")
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("actual lock contention");
            cancelled.store(true, Ordering::Release);
            assert_eq!(
                waiting.join().expect("waiter thread").kind(),
                ErrorKind::Interrupted
            );
        });
        cancelled.store(false, Ordering::Release);
        assert!(matches!(
            fixture.registry.lease(&binding, Duration::ZERO, &cancelled),
            Err(error) if error.kind() == ErrorKind::TimedOut
        ));
        drop(lease);
        let _lease = fixture
            .registry
            .lease(&binding, MAX_WAIT, &cancelled)
            .expect("same inode reused");
        assert_eq!(
            before,
            std::fs::metadata(fixture.named(&binding))
                .expect("lock survives")
                .ino()
        );
        assert!(
            fixture
                .registry
                .lease(&binding, MAX_WAIT + Duration::from_secs(1), &cancelled)
                .is_err()
        );
    }

    #[test]
    fn admission_uses_an_independent_open_description_even_in_one_process() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        fs::flock(
            &fixture.registry.directory,
            FlockOperation::NonBlockingLockExclusive,
        )
        .expect("held admission");
        let cancelled = AtomicBool::new(false);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                fixture
                    .registry
                    .lease_with(&binding, MAX_WAIT, &cancelled, |stage| {
                        if stage == WaitFor::Registry {
                            let _ = send.try_send(());
                        }
                    })
                    .is_ok()
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("admission really contends");
            assert!(!fixture.named(&binding).exists());
            fs::flock(&fixture.registry.directory, FlockOperation::Unlock)
                .expect("release admission");
            assert!(waiting.join().expect("admitted waiter"));
        });
    }

    #[test]
    fn unsafe_registry_objects_and_inheritable_descriptors_are_refused() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let named = fixture.named(&binding);
        let outside = fixture.root.join("outside");
        std::fs::write(&outside, b"").expect("outside file");
        let cancelled = AtomicBool::new(false);
        std::os::unix::fs::symlink(&outside, &named).expect("symlink lock");
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::InvalidData
        ));
        std::fs::remove_file(&named).expect("remove fixture link");
        std::fs::hard_link(&outside, &named).expect("hardlinked lock");
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::InvalidData
        ));
        std::fs::remove_file(&named).expect("remove fixture hardlink");
        std::fs::create_dir(&named).expect("directory lock");
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::InvalidData
        ));
        std::fs::remove_dir(&named).expect("remove fixture directory");
        fs::mknodat(fs::CWD, &named, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0)
            .expect("FIFO fixture");
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::InvalidData
        ));
        std::fs::remove_file(&named).expect("remove fixture FIFO");
        std::fs::write(&named, b"not a lock").expect("nonempty lock");
        std::fs::set_permissions(&named, std::fs::Permissions::from_mode(0o600))
            .expect("private file");
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::InvalidData
        ));
        std::fs::write(&named, b"").expect("empty lock");
        std::fs::set_permissions(&named, std::fs::Permissions::from_mode(0o644))
            .expect("unsafe mode");
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::InvalidData
        ));
        let inherited = directory(&fixture.root.join("registry"));
        rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty()).expect("inheritable fd");
        assert!(Registry::from_private(inherited).is_err());
        std::fs::set_permissions(
            fixture.root.join("registry"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("public registry");
        assert!(Registry::from_private(directory(&fixture.root.join("registry"))).is_err());
        assert_eq!(std::fs::read(&outside).expect("outside unchanged"), b"");
    }

    #[test]
    fn capacity_refuses_new_keys_without_evicting_existing_locks() {
        let fixture = Fixture::new();
        let first = binding(&fixture.target("existing"));
        let cancelled = AtomicBool::new(false);
        drop(
            fixture
                .registry
                .lease(&first, Duration::ZERO, &cancelled)
                .expect("first lock"),
        );
        for n in 1..MAX_ENTRIES {
            let name = format!("{n:064x}");
            assert_ne!(name, first.key());
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(fixture.root.join("registry").join(name))
                .expect("capacity fixture");
        }
        let another = binding(&fixture.target("new"));
        assert!(matches!(
            fixture.registry.lease(&another, MAX_WAIT, &cancelled),
            Err(error) if error.to_string().contains("capacity")
        ));
        assert!(!fixture.named(&another).exists());
        let _lease = fixture
            .registry
            .lease(&first, MAX_WAIT, &cancelled)
            .expect("existing key usable");
        assert_eq!(
            std::fs::read_dir(fixture.root.join("registry"))
                .expect("registry")
                .count(),
            MAX_ENTRIES
        );
    }

    #[test]
    fn lease_registry_and_parent_descriptors_do_not_survive_exec() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        let lease = fixture
            .registry
            .lease(&binding, Duration::ZERO, &cancelled)
            .expect("lease");
        let mut forbidden = Vec::new();
        for fd in [&fixture.registry.directory, &binding._parent, &lease._file] {
            cloexec(fd).expect("close-on-exec");
            let metadata = fs::fstat(fd).expect("descriptor binding");
            forbidden.push(format!("{}:{}", metadata.st_dev, metadata.st_ino));
        }
        let mut command = probe_command("fds");
        command.env("CASPER_FILE_LEASE_FORBIDDEN", forbidden.join(","));
        Process(command.spawn().expect("exec probe")).finish();
    }

    #[test]
    fn registry_privacy_drift_while_waiting_refuses_the_lease() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        let held = fixture
            .registry
            .lease(&binding, Duration::ZERO, &cancelled)
            .expect("holder");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                fixture
                    .registry
                    .lease_with(&binding, MAX_WAIT, &cancelled, |stage| {
                        if stage == WaitFor::Target {
                            let _ = send.try_send(());
                        }
                    })
                    .err()
                    .map(|error| error.kind())
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("contended lease");
            std::fs::set_permissions(
                fixture.root.join("registry"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("privacy drift");
            drop(held);
            assert_eq!(
                waiting.join().expect("waiter"),
                Some(ErrorKind::InvalidData)
            );
        });
    }

    #[test]
    fn a_replaced_lock_name_never_admits_a_waiter_on_the_old_inode() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        let held = fixture
            .registry
            .lease(&binding, Duration::ZERO, &cancelled)
            .expect("holder");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                fixture
                    .registry
                    .lease_with(&binding, MAX_WAIT, &cancelled, |stage| {
                        if stage == WaitFor::Target {
                            let _ = send.try_send(());
                        }
                    })
                    .err()
                    .map(|error| error.kind())
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("contended lease");
            std::fs::rename(fixture.named(&binding), fixture.root.join("old-lock"))
                .expect("move fixture lock");
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(fixture.named(&binding))
                .expect("replace fixture lock");
            drop(held);
            assert_eq!(
                waiting.join().expect("waiter"),
                Some(ErrorKind::InvalidData)
            );
        });
    }

    #[test]
    fn cancellation_before_admission_creates_no_lock_entry() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            fixture.registry.lease(&binding, MAX_WAIT, &cancelled),
            Err(error) if error.kind() == ErrorKind::Interrupted
        ));
        assert!(
            fixture
                .registry
                .lease(&binding, MAX_WAIT + Duration::from_secs(1), &cancelled)
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(fixture.root.join("registry"))
                .expect("empty registry")
                .count(),
            0
        );
    }

    #[test]
    fn a_deleted_parent_cannot_receive_a_lease_after_waiting() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        let held = fixture
            .registry
            .lease(&binding, Duration::ZERO, &cancelled)
            .expect("holder");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                fixture
                    .registry
                    .lease_with(&binding, MAX_WAIT, &cancelled, |stage| {
                        if stage == WaitFor::Target {
                            let _ = send.try_send(());
                        }
                    })
                    .err()
                    .map(|error| error.kind())
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("contended lease");
            std::fs::remove_dir(fixture.root.join("work")).expect("remove empty source parent");
            drop(held);
            assert_eq!(
                waiting.join().expect("waiter"),
                Some(ErrorKind::InvalidData)
            );
        });
    }

    #[test]
    fn path_descriptors_are_normalized_and_unknown_registry_entries_refuse_new_keys() {
        let fixture = Fixture::new();
        let fd = fs::open(
            fixture.root.join("registry"),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("pinned directory");
        let registry = Registry::from_private(fd).expect("normalized registry");
        let bound = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        drop(
            registry
                .lease(&bound, MAX_WAIT, &cancelled)
                .expect("first key"),
        );
        std::fs::write(fixture.root.join("registry/unrecognized"), b"leave intact")
            .expect("unknown entry");
        let other = binding(&fixture.target("other"));
        assert!(matches!(registry.lease(&other, MAX_WAIT, &cancelled),
            Err(error) if error.to_string().contains("unexpected entry")
        ));
        assert!(!fixture.named(&other).exists());
        assert_eq!(
            std::fs::read(fixture.root.join("registry/unrecognized")).expect("left intact"),
            b"leave intact"
        );
        drop(
            registry
                .lease(&bound, MAX_WAIT, &cancelled)
                .expect("existing key still usable"),
        );
    }

    #[test]
    fn privacy_drift_during_admission_creates_no_lock_entry() {
        let fixture = Fixture::new();
        let binding = binding(&fixture.target("file"));
        let cancelled = AtomicBool::new(false);
        fs::flock(
            &fixture.registry.directory,
            FlockOperation::NonBlockingLockExclusive,
        )
        .expect("admission held");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                fixture
                    .registry
                    .lease_with(&binding, MAX_WAIT, &cancelled, |stage| {
                        if stage == WaitFor::Registry {
                            let _ = send.try_send(());
                        }
                    })
                    .err()
                    .map(|error| error.kind())
            });
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("admission contended");
            std::fs::set_permissions(
                fixture.root.join("registry"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("privacy drift");
            fs::flock(&fixture.registry.directory, FlockOperation::Unlock)
                .expect("release admission");
            assert_eq!(
                waiting.join().expect("waiter"),
                Some(ErrorKind::InvalidData)
            );
            assert!(!fixture.named(&binding).exists());
        });
    }

    #[test]
    fn interrupted_lock_attempts_still_obey_deadlines_and_cancellation() {
        let cancelled = AtomicBool::new(false);
        let mut attempts = 0;
        let outcome = lock_using(Instant::now(), &cancelled, &mut || {}, || {
            attempts += 1;
            if attempts == 1 {
                Err(rustix::io::Errno::INTR)
            } else {
                Ok(())
            }
        });
        assert!(matches!(outcome, Err(error) if error.kind() == ErrorKind::TimedOut));
        assert_eq!(attempts, 1, "expired interrupted calls must not be retried");
        attempts = 0;
        let outcome = lock_using(Instant::now() + MAX_WAIT, &cancelled, &mut || {}, || {
            attempts += 1;
            cancelled.store(true, Ordering::Release);
            Err(rustix::io::Errno::INTR)
        });
        assert!(matches!(outcome, Err(error) if error.kind() == ErrorKind::Interrupted));
        assert_eq!(attempts, 1);
    }
}
