use super::parallel::{Client, completed};
use super::{Fixture, Process, TOOLS, install, mutation, read, result};
use serde_json::json;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;

const PROBE: &str = "admission::kernel_failures::kernel_probe";

fn helper() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    let path = exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("selected test profile")
        .join("examples/file-fault-exec");
    assert!(
        path.is_file(),
        "owner recipe must build the fault executor: {}",
        path.display()
    );
    path
}

#[test]
fn kernel_probe() {
    let Some(binary) = std::env::var_os("CASPER_KERNEL_BINARY") else {
        return;
    };
    let door = std::env::var("CASPER_KERNEL_DOOR").expect("selected door");
    let mode = std::env::var("CASPER_KERNEL_FAILURE").expect("selected kernel failure");
    let binary = PathBuf::from(binary);
    let client = Client::new(binary.clone(), door == "library");
    let source = "b".repeat(4096);
    std::fs::write("source", &source).expect("source on fixture filesystem");
    let before = std::fs::metadata("source").expect("original inode");
    let read_reply = client.call(read("source"));
    let observation = &result(&read_reply)["observations"][0];
    let expected = observation["revision"]
        .as_str()
        .expect("actual source revision");
    let filesystem = rustix::fs::statfs(".").expect("fixture filesystem capacity");
    let restricted = if mode == "sync" {
        Some(Client::with_fault(
            binary,
            door == "library",
            Some(helper()),
        ))
    } else {
        assert_eq!(mode, "space");
        assert_eq!(filesystem.f_type, libc::TMPFS_MAGIC);
        let free = filesystem.f_bavail as u64 * filesystem.f_bsize as u64;
        assert!(
            free > 0 && free < 64 * 1024,
            "fixture must have partial staging capacity: {free}"
        );
        None
    };
    let failing = restricted.as_ref().unwrap_or(&client);
    let failed = failing.call(mutation(
        "store",
        json!({"path":"source","contents":"x".repeat(64 * 1024)}),
        expected,
        "failed-kernel-stage",
    ));
    let ran = result(&failed);
    assert_eq!(ran["failed"], true, "{failed}");
    assert_eq!(ran["mutation"]["id"], "failed-kernel-stage", "{failed}");
    assert_eq!(ran["mutation"]["outcome"], "refused", "{failed}");
    let errno = if mode == "sync" {
        libc::EIO
    } else {
        libc::ENOSPC
    };
    assert!(
        ran["said"]
            .as_str()
            .expect("actual kernel refusal")
            .contains(&format!("os error {errno}")),
        "{failed}"
    );
    assert!(
        ran.get("observations").is_none(),
        "failed staging produced an observation: {failed}"
    );
    assert_eq!(
        std::fs::read("source").expect("unchanged source"),
        source.as_bytes()
    );
    let after = std::fs::metadata("source").expect("unchanged inode");
    assert_eq!(
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.uid(),
            after.gid()
        ),
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.uid(),
            before.gid()
        )
    );
    let entries: Vec<_> = std::fs::read_dir(".")
        .expect("source directory")
        .map(|entry| entry.expect("source entry").file_name())
        .collect();
    assert_eq!(
        entries,
        vec![std::ffi::OsString::from("source")],
        "owned stage was not cleaned"
    );
    if mode == "space" {
        let after = rustix::fs::statfs(".").expect("capacity after failed staging");
        assert_eq!(
            after.f_bavail, filesystem.f_bavail,
            "anonymous stage retained filesystem capacity"
        );
    }
    let recovered = client.call(mutation(
        "store",
        json!({"path":"source","contents":"recovered\n"}),
        expected,
        "after-kernel-refusal",
    ));
    assert_eq!(
        result(&recovered)["mutation"]["outcome"],
        "committed",
        "{recovered}"
    );
    assert_eq!(result(&recovered)["mutation"]["id"], "after-kernel-refusal");
    assert_eq!(
        std::fs::read("source").expect("successor after refused staging"),
        b"recovered\n"
    );
    assert_eq!(
        result(&recovered)["observations"][0]["path_key"],
        observation["path_key"]
    );
    assert_ne!(
        result(&recovered)["observations"][0]["revision"],
        observation["revision"]
    );
}

fn exercise(mode: &str) {
    for backend in [false, true] {
        for door in ["command", "library"] {
            let fixture = Fixture::new(backend);
            install(&fixture, TOOLS);
            let mut command = fixture.command_program_with_work_limit(
                true,
                None,
                &std::env::current_exe().expect("probe executable"),
                (mode == "space").then_some(64 * 1024),
            );
            command
                .args(["--exact", PROBE, "--nocapture"])
                .env("CASPER_KERNEL_BINARY", &fixture.binary)
                .env("CASPER_KERNEL_DOOR", door)
                .env("CASPER_KERNEL_FAILURE", mode);
            let output = completed(Process(
                command.spawn().expect("actual kernel failure probe"),
            ));
            assert!(String::from_utf8_lossy(&output).contains(&format!("test {PROBE} ... ok")));
        }
    }
}

#[test]
fn actual_native_enospc_preserves_target_cleans_stages_and_releases_leases_on_both_doors_and_backends()
 {
    exercise("space");
}

#[test]
fn actual_native_data_sync_failure_preserves_target_and_releases_leases_on_both_doors_and_backends()
{
    exercise("sync");
}
