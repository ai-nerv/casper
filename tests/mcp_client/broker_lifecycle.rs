use super::*;
use casper::files::locking::{Binding, Registry};
use rustix::fs::{Mode, OFlags};
use std::os::unix::fs::PermissionsExt as _;

fn acquisition_processes(work: &std::path::Path) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .expect("proc")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            if std::fs::read_link(entry.path().join("cwd")).ok()?.as_path() != work {
                return None;
            }
            let args = std::fs::read(entry.path().join("cmdline")).ok()?;
            args.split(|byte| *byte == 0)
                .any(|arg| matches!(arg, b"--mcp-read-acquire" | b"--file-worker"))
                .then_some(pid)
        })
        .collect()
}

fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !stat
            .split_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    })
}

#[tokio::test]
async fn broker_deadlines_stop_owned_acquisition_workers_without_releasing_another_lease_on_both_backends()
 {
    for bubblewrap in [true, false] {
        let dir = fixture();
        let _namespace = NativeNamespace::new(&dir, &dir.join("work"));
        let mut config = super::broker::contained_server(&dir, "read");
        config["mcp_servers"]["nested"]["timeout_secs"] = json!(1);
        let registry = dir.join("var-tmp/.casper-files-v1-0");
        std::fs::create_dir(&registry).expect("private registry");
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o700))
            .expect("registry mode");
        let open_dir = |path: &std::path::Path| {
            rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .expect("directory fd")
        };
        let domain =
            Registry::from_private(open_dir(&registry)).expect("private registry validation");
        let binding = Binding::from_parent(
            open_dir(&dir.join("work")),
            std::ffi::OsStr::new("proof.txt"),
        )
        .expect("source binding");
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let lease = domain
            .lease(&binding, Duration::from_secs(1), &cancelled)
            .expect("held lease");
        let lock_inode = std::fs::metadata(registry.join(binding.key())).expect("lock inode");
        let call = Some(json!({"tool":"mcp__nested__read","args":{"path":"proof.txt"}}));
        let work = fixture_support::invoked_on(&dir, &config, "run", call, Some("1"), bubblewrap);
        tokio::pin!(work);
        let mut observed = std::collections::BTreeSet::new();
        let (reply, stderr) = loop {
            tokio::select! {
                reply = &mut work => break reply,
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    observed.extend(acquisition_processes(&dir.join("work")));
                }
            }
        };
        assert!(
            !observed.is_empty(),
            "no actual acquisition worker was observed: {reply} {stderr}"
        );
        assert_eq!(reply["result"][0]["failed"], true, "{reply}");
        assert!(
            reply["result"][0]["said"]
                .as_str()
                .expect("timeout reason")
                .contains("deadline"),
            "{reply}"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while observed.iter().copied().any(running) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned workers stop after broker timeout");
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(
            std::fs::metadata(registry.join(binding.key()))
                .expect("persistent lock")
                .ino(),
            lock_inode.ino()
        );
        assert!(
            domain
                .lease(&binding, Duration::from_millis(50), &cancelled)
                .is_err(),
            "timeout released another holder's lease"
        );
        drop(lease);
        assert!(
            domain
                .lease(&binding, Duration::from_secs(1), &cancelled)
                .is_ok()
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("work/proof.txt")).expect("unchanged"),
            "External MCP reached a real tool.\n"
        );
    }
}
