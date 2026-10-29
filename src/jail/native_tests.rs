#[cfg(test)]
mod cases {
    #![allow(clippy::expect_used)]

    use super::super::*;
    use crate::files::{control, locking};
    use crate::scratch::Scratch;
    use rustix::fs::{self, Mode, OFlags};
    use std::io::IoSlice;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    const PROBE: &str = "jail::native_tests::cases::probe";

    fn directory(path: &Path) -> OwnedFd {
        fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("directory")
    }

    fn executable(name: &str) -> PathBuf {
        std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .map(|path| path.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| panic!("missing prerequisite {name}"))
    }

    struct Process(Child);

    impl Process {
        fn wait(&mut self) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = self.0.try_wait().expect("driver status") {
                    return status;
                }
                assert!(Instant::now() < deadline, "native IPC driver timeout");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn finish(&mut self) {
            let status = self.wait();
            assert!(status.success(), "native IPC driver failed: {status}");
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn roundtrip(
        bubblewrap: bool,
        native: bool,
        reach: bool,
        staging: bool,
        lifecycle: Option<(&str, bool)>,
    ) {
        let root = Scratch::new("native-ipc", "backend");
        for name in [
            "work", "home", "config", "data", "state", "cache", "tmp", "registry", "bin",
        ] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(name))
                .expect("private fixture");
        }
        std::fs::create_dir(root.join("home/.ssh")).expect("credential store");
        std::fs::write(root.join("home/.ssh/id"), b"FIXTURE_ONLY_SECRET")
            .expect("fixture credential");
        std::fs::write(root.join("work/source.txt"), b"native snapshot\n").expect("source");
        let registry =
            locking::Registry::from_private(directory(&root.join("registry"))).expect("registry");
        let binding = locking::Binding::from_parent(
            directory(&root.join("work")),
            std::ffi::OsStr::new("source.txt"),
        )
        .expect("expected source binding");
        drop(
            registry
                .delegate(
                    binding.identity(),
                    locking::MAX_WAIT,
                    &AtomicBool::new(false),
                )
                .expect("seed lock inode"),
        );
        let metadata =
            std::fs::metadata(root.join("registry").join(binding.key())).expect("lease identity");
        let mut paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .filter(|path| !path.join("bwrap").is_file())
            .collect();
        if bubblewrap {
            let launcher = executable("bwrap");
            let metadata = std::fs::metadata(&launcher).expect("native recipe backend");
            assert_eq!(
                metadata.mode() & 0o222,
                0,
                "backend must be captured before parallel tests"
            );
            assert_eq!(metadata.nlink(), 1, "backend has hardlink aliases");
            paths.insert(0, launcher.parent().expect("backend parent").to_owned());
        }
        let (coordinator, worker) = control::pair().expect("private socket");
        let socket_metadata = fs::fstat(&worker).expect("private channel identity");
        let registry_metadata =
            std::fs::metadata(root.join("registry")).expect("registry identity");
        let mut child = Process(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", PROBE, "--nocapture"])
                .env_clear()
                .env("PATH", std::env::join_paths(paths).expect("backend PATH"))
                .env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_STATE_HOME", root.join("state"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env("TMPDIR", root.join("tmp"))
                .env("CASPER_NATIVE_TEST_ROLE", "driver")
                .env("CASPER_NATIVE_TEST_ROOT", root.as_os_str())
                .env("CASPER_NATIVE_TEST_MODE", if native { "1" } else { "0" })
                .env("CASPER_NATIVE_TEST_REACH", if reach { "1" } else { "0" })
                .env("CASPER_NATIVE_TEST_STAGE", if staging { "1" } else { "0" })
                .env(
                    "CASPER_NATIVE_TEST_LIFECYCLE",
                    lifecycle.map_or("none", |(edge, _)| edge),
                )
                .env(
                    "CASPER_NATIVE_TEST_FORBIDDEN",
                    format!(
                        "{}:{},{}:{},{}:{}",
                        metadata.dev(),
                        metadata.ino(),
                        socket_metadata.st_dev,
                        socket_metadata.st_ino,
                        registry_metadata.dev(),
                        registry_metadata.ino()
                    ),
                )
                .stdin(Stdio::from(worker))
                .stdout(Stdio::null())
                .spawn()
                .expect("native backend driver"),
        );
        if native {
            control::send_request(coordinator.as_fd(), &vec![b'x'; 300 * 1024 + 1])
                .expect("sealed large request through actual backend");
            let key =
                control::receive_binding(coordinator.as_fd()).expect("jailed binding request");
            assert_eq!(
                key.as_str(),
                binding.key(),
                "namespace changed binding identity"
            );
            let lease = registry
                .delegate(&key, locking::MAX_WAIT, &AtomicBool::new(false))
                .expect("native lease");
            lease
                .handoff(coordinator.as_fd())
                .expect("pass only held lock capability");
            drop(lease);
        }
        if let Some((edge, kill_coordinator)) = lifecycle {
            let (marker, pid) = control::receive_lifecycle_probe(coordinator.as_fd())
                .expect("actual worker reached transaction barrier");
            assert_eq!(
                marker,
                match edge {
                    "snapshot_verified" => 4,
                    "staged" => 1,
                    "before_commit" => 2,
                    "committed" => 3,
                    "directory_sync" => 5,
                    _ => panic!("unknown edge"),
                }
            );
            assert!(
                registry
                    .delegate(binding.identity(), Duration::ZERO, &AtomicBool::new(false))
                    .is_err(),
                "staged worker lost its actual kernel lease"
            );
            if kill_coordinator {
                child.0.kill().expect("kill native coordinator");
            } else {
                rustix::process::pidfd_send_signal(&pid, rustix::process::Signal::KILL)
                    .expect("kill exact staged worker");
            }
            let _ = child.wait();
            let held = registry
                .delegate(
                    binding.identity(),
                    locking::MAX_WAIT,
                    &AtomicBool::new(false),
                )
                .expect("lease released after actual worker death");
            let mut terminal = [rustix::event::PollFd::new(
                &pid,
                rustix::event::PollFlags::IN,
            )];
            assert_eq!(
                rustix::event::poll(
                    &mut terminal,
                    Some(&rustix::event::Timespec {
                        tv_sec: 1,
                        tv_nsec: 0
                    })
                )
                .expect("pidfd death proof"),
                1,
                "killed worker did not become terminal"
            );
            let expected = if matches!(edge, "committed" | "directory_sync") {
                b"atomic lifecycle\n".as_slice()
            } else {
                b"native snapshot\n".as_slice()
            };
            assert_eq!(
                std::fs::read(root.join("work/source.txt")).expect("target after death"),
                expected
            );
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(
                std::fs::read(root.join("work/source.txt")).expect("no later commit"),
                expected
            );
            let orphans = std::fs::read_dir(root.join("work"))
                .expect("stage accounting")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .as_encoded_bytes()
                        .starts_with(b".casper-stage-")
                })
                .count();
            assert_eq!(
                orphans,
                usize::from(edge == "before_commit"),
                "anonymous stages must vanish; named crash remnants must not be guessed away"
            );
            drop(held);
        } else {
            child.finish();
        }
        drop(
            registry
                .delegate(
                    binding.identity(),
                    locking::MAX_WAIT,
                    &AtomicBool::new(false),
                )
                .expect("jailed worker released its lease"),
        );
    }

    #[test]
    fn native_control_passes_a_lease_without_registry_access_in_both_backends() {
        for backend in [false, true] {
            for reach in [false, true] {
                roundtrip(backend, true, reach, false, None);
            }
        }
    }

    #[test]
    fn ordinary_commands_cannot_use_the_native_control_exception_in_either_backend() {
        for backend in [false, true] {
            roundtrip(backend, false, false, false, None);
        }
    }

    #[test]
    fn native_anonymous_stages_publish_without_extra_grants_in_both_backends() {
        for backend in [false, true] {
            roundtrip(backend, true, false, true, None);
        }
    }

    #[test]
    fn killed_staged_workers_and_coordinators_never_outlive_their_transaction_lease() {
        for backend in [false, true] {
            for edge in [
                "snapshot_verified",
                "staged",
                "before_commit",
                "committed",
                "directory_sync",
            ] {
                for coordinator in [false, true] {
                    roundtrip(backend, true, false, false, Some((edge, coordinator)));
                }
            }
        }
    }

    fn stage_probe(root: &Path) {
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;
        let parent = directory(&root.join("work"));
        let fd = fs::openat(
            &parent,
            ".",
            OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .expect("anonymous stage in actual jail");
        let mut file = std::fs::File::from(fd);
        file.write_all(b"complete stage\n").expect("stage data");
        assert_eq!(fs::fstat(&file).expect("stage metadata").st_nlink, 0);
        file.sync_all().expect("stage data sync");
        fs::linkat(
            fs::CWD,
            format!("/proc/self/fd/{}", file.as_raw_fd()),
            &parent,
            "stage",
            fs::AtFlags::SYMLINK_FOLLOW,
        )
        .expect("anonymous stage publication in actual jail");
        assert_eq!(fs::fstat(&file).expect("linked stage").st_nlink, 1);
        assert_eq!(
            fs::renameat_with(
                &parent,
                "stage",
                &parent,
                "source.txt",
                fs::RenameFlags::NOREPLACE
            )
            .err(),
            Some(rustix::io::Errno::EXIST)
        );
        assert_eq!(
            std::fs::read("source.txt").expect("source intact"),
            b"native snapshot\n"
        );
        fs::renameat(&parent, "stage", &parent, "published").expect("stage rename");
        fs::fsync(&parent).expect("directory sync");
        assert_eq!(
            std::fs::read("published").expect("published data"),
            b"complete stage\n"
        );
        fs::unlinkat(&parent, "published", fs::AtFlags::empty()).expect("fixture stage cleanup");
        crate::files::worker::stage_closure_probe("source.txt")
            .expect("actual native Stage/source/parent close on exec");
    }

    fn descriptor_probe() {
        let forbidden = std::env::var("CASPER_NATIVE_TEST_FORBIDDEN").expect("lock identity");
        std::fs::metadata("/proc/self/fd/1").expect("known fd metadata must remain inspectable");
        for n in 0..256 {
            match std::fs::metadata(format!("/proc/self/fd/{n}")) {
                Ok(metadata) => assert!(
                    !forbidden
                        .split(',')
                        .any(|value| value == format!("{}:{}", metadata.dev(), metadata.ino())),
                    "native lease, registry or private channel descriptor survived exec"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot prove descriptor closure: {error}"),
            }
        }
    }

    #[test]
    fn probe() {
        let Ok(role) = std::env::var("CASPER_NATIVE_TEST_ROLE") else {
            return;
        };
        if role == "fds" {
            descriptor_probe();
            return;
        }
        let root =
            PathBuf::from(std::env::var_os("CASPER_NATIVE_TEST_ROOT").expect("fixture root"));
        let native = std::env::var("CASPER_NATIVE_TEST_MODE").expect("mode") == "1";
        if role == "driver" {
            let reach = std::env::var("CASPER_NATIVE_TEST_REACH").expect("reach profile") == "1";
            let jail = Jail {
                grants: Ok(Some(Grants {
                    reach,
                    ..Grants::default()
                })),
                cwd: root.join("work"),
                home: root.join("home"),
            };
            let executable = std::env::current_exe().expect("native test worker");
            let args = [
                "--exact".to_owned(),
                PROBE.to_owned(),
                "--nocapture".to_owned(),
            ];
            let mut prepared = jail
                .prepare_inner(
                    executable.to_str().expect("worker path"),
                    &args,
                    None,
                    &[],
                    false,
                    native,
                )
                .expect("actual backend preparation");
            prepared.inner(&[
                ("CASPER_NATIVE_TEST_ROLE".into(), "worker".into()),
                ("CASPER_NATIVE_TEST_ROOT".into(), root.display().to_string()),
                (
                    "CASPER_NATIVE_TEST_MODE".into(),
                    if native { "1" } else { "0" }.into(),
                ),
                (
                    "CASPER_NATIVE_TEST_FORBIDDEN".into(),
                    std::env::var("CASPER_NATIVE_TEST_FORBIDDEN").expect("identity"),
                ),
                (
                    "CASPER_NATIVE_TEST_STAGE".into(),
                    std::env::var("CASPER_NATIVE_TEST_STAGE").expect("stage mode"),
                ),
                (
                    "CASPER_NATIVE_TEST_LIFECYCLE".into(),
                    std::env::var("CASPER_NATIVE_TEST_LIFECYCLE").expect("lifecycle mode"),
                ),
            ]);
            let input = std::io::stdin();
            let socket = rustix::io::fcntl_dupfd_cloexec(input.as_fd(), 3)
                .expect("private channel duplicate");
            let status = prepared
                .command()
                .stdin(Stdio::from(socket))
                .status()
                .expect("native worker");
            if std::env::var("CASPER_NATIVE_TEST_LIFECYCLE").as_deref() == Ok("none") {
                assert!(status.success(), "actual native backend failed: {status}");
            } else {
                assert!(
                    status.signal() == Some(9) || status.code() == Some(137),
                    "lifecycle worker did not terminate at SIGKILL barrier: {status}"
                );
            }
            assert!(
                prepared.restored().is_empty(),
                "native read changed protected paths"
            );
            return;
        }
        assert_eq!(role, "worker");
        let binding = locking::Binding::from_parent(
            directory(&root.join("work")),
            std::ffi::OsStr::new("source.txt"),
        )
        .expect("binding resolved inside actual jail");
        let stdin = std::io::stdin();
        if native {
            assert_eq!(
                control::receive_request(stdin.as_fd())
                    .expect("sealed request without filesystem grant"),
                vec![b'x'; 300 * 1024 + 1]
            );
        }
        let sent = control::send_binding(stdin.as_fd(), &binding);
        if !native {
            assert!(
                matches!(sent, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied),
                "ordinary command gained native sendmsg"
            );
            return;
        }
        sent.expect("native stdin channel alone may sendmsg");
        let _lease =
            control::receive_lease(stdin.as_fd(), &binding).expect("held descriptor inside jail");
        let lifecycle = std::env::var("CASPER_NATIVE_TEST_LIFECYCLE").expect("lifecycle mode");
        if lifecycle != "none" {
            crate::files::worker::lifecycle_probe("source.txt", &_lease, stdin.as_fd(), &lifecycle)
                .expect("lifecycle worker must die at its barrier");
        }
        assert_eq!(
            std::fs::read("source.txt").expect("jailed source"),
            b"native snapshot\n"
        );
        if std::env::var("CASPER_NATIVE_TEST_STAGE").as_deref() == Ok("1") {
            stage_probe(&root);
        }
        for denied in [root.join("registry"), root.join("home/.ssh")] {
            assert!(
                std::fs::read_dir(denied).is_err(),
                "native IPC widened filesystem access"
            );
        }
        assert!(
            matches!(
                rustix::net::socket(
                    rustix::net::AddressFamily::INET,
                    rustix::net::SocketType::STREAM,
                    None
                ),
                Err(rustix::io::Errno::PERM)
            ),
            "native IPC enabled network sockets"
        );
        let stdout = std::io::stdout();
        let mut ancillary = rustix::net::SendAncillaryBuffer::default();
        assert!(
            matches!(
                rustix::net::sendmsg(
                    stdout.as_fd(),
                    &[IoSlice::new(b"not control")],
                    &mut ancillary,
                    rustix::net::SendFlags::NOSIGNAL
                ),
                Err(rustix::io::Errno::PERM)
            ),
            "native exception was not confined to stdin"
        );
        let status = Command::new(std::env::current_exe().expect("exec probe"))
            .args(["--exact", PROBE, "--nocapture"])
            .env("CASPER_NATIVE_TEST_ROLE", "fds")
            .stdout(Stdio::null())
            .status()
            .expect("jailed exec probe");
        assert!(
            status.success(),
            "cannot prove native fd closure under this backend"
        );
    }

    #[test]
    fn native_control_refuses_off_profiles_and_terminal_children() {
        let cwd = std::env::current_dir().expect("cwd");
        for (grants, terminal) in [(None, false), (Some(Grants::default()), true)] {
            let jail = Jail {
                grants: Ok(grants),
                cwd: cwd.clone(),
                home: PathBuf::new(),
            };
            let refused = jail
                .prepare_inner("not-executed", &[], None, &[], terminal, true)
                .err()
                .expect("unsupported native containment must refuse");
            assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
        }
    }
}
