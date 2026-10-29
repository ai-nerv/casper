use super::parallel::{Client, completed};
use super::{Fixture, Process, TOOLS, install, mutation, read, result};
use serde_json::{Value, json};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;

const PROBE: &str = "admission::metadata::metadata_probe";

fn acl(named: bool) -> Vec<u8> {
    let mut bytes = 2u32.to_le_bytes().to_vec();
    let mut entries = vec![(1u16, 7u16, u32::MAX)];
    if named {
        entries.push((2, 4, rustix::process::geteuid().as_raw()));
    }
    entries.push((4, 0, u32::MAX));
    if named {
        entries.push((16, 4, u32::MAX));
    }
    entries.push((32, 0, u32::MAX));
    for (tag, permission, id) in entries {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&permission.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    bytes
}

fn attribute(path: &str, name: &str, expected: &[u8]) {
    let mut bytes = [0u8; 256];
    let size = rustix::fs::getxattr(path, name, &mut bytes[..]).expect("ACL retained");
    assert_eq!(&bytes[..size], expected);
}

fn refused(reply: &Value, id: &str, reason: &str) {
    let ran = result(reply);
    assert_eq!(ran["failed"], true, "{reply}");
    assert_eq!(ran["mutation"]["outcome"], "refused", "{reply}");
    assert_eq!(ran["mutation"]["id"], id, "{reply}");
    assert!(
        ran["said"]
            .as_str()
            .expect("policy refusal")
            .contains(reason),
        "{reply}"
    );
    assert!(ran.get("observations").is_none(), "{reply}");
}

#[test]
fn metadata_probe() {
    let Some(binary) = std::env::var_os("CASPER_METADATA_BINARY") else {
        return;
    };
    let mode = std::env::var("CASPER_METADATA_CASE").expect("metadata case");
    let socket = std::env::var("CASPER_METADATA_DOOR").expect("metadata door") == "library";
    let client = Client::new(PathBuf::from(binary), socket);
    let original = b"before\n";
    std::fs::write("source", original).expect("private source");
    if mode == "sparse" {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open("source")
            .expect("sparse source");
        file.set_len(16 * 1024).expect("sparse extent");
        file.write_all(b"HEAD").expect("sparse prefix");
        let before = std::fs::metadata("source").expect("sparse metadata");
        assert!(
            before.blocks() * 512 < before.len(),
            "fixture must contain actual holes"
        );
        let bytes = std::fs::read("source").expect("sparse bytes");
        let observed = client.call(read("source"));
        assert_ne!(result(&observed)["failed"], true, "{observed}");
        assert!(
            result(&observed).get("observations").is_none(),
            "{observed}"
        );
        let reply = client.call(mutation(
            "store",
            json!({"path":"source","contents":"unsafe"}),
            &format!("cfr1:{}", "0".repeat(64)),
            "sparse-refusal",
        ));
        refused(&reply, "sparse-refusal", "UTF-8 without NUL");
        let after = std::fs::metadata("source").expect("sparse unchanged");
        assert_eq!(
            (after.ino(), after.blocks(), after.len()),
            (before.ino(), before.blocks(), before.len())
        );
        assert_eq!(std::fs::read("source").expect("sparse retained"), bytes);
        return;
    }
    if mode == "timestamps" {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open("source")
            .expect("source times");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10);
        file.set_times(
            std::fs::FileTimes::new()
                .set_accessed(old)
                .set_modified(old),
        )
        .expect("old source times");
        std::fs::set_permissions("source", std::fs::Permissions::from_mode(0o751))
            .expect("ordinary executable mode");
        let before = std::fs::metadata("source").expect("original metadata");
        let observed = client.call(read("source"));
        let expected = result(&observed)["observations"][0]["revision"]
            .as_str()
            .expect("actual revision");
        let reply = client.call(mutation(
            "change",
            json!({"path":"source","old":"before","new":"after"}),
            expected,
            "staged-times",
        ));
        assert_eq!(
            result(&reply)["mutation"]["outcome"],
            "committed",
            "{reply}"
        );
        let after = std::fs::metadata("source").expect("new inode metadata");
        assert_ne!(after.ino(), before.ino());
        assert_eq!(
            (after.mode(), after.uid(), after.gid()),
            (before.mode(), before.uid(), before.gid())
        );
        assert!(
            after.mtime() > before.mtime(),
            "stage must not restore stale source write times"
        );
        assert_eq!(
            std::fs::read("source").expect("committed bytes"),
            b"after\n"
        );
        assert_ne!(result(&reply)["observations"][0]["revision"], expected);
        return;
    }
    assert!(matches!(mode.as_str(), "access-acl" | "default-acl"));
    let (path, name) = if mode == "access-acl" {
        ("source", "system.posix_acl_access")
    } else {
        (".", "system.posix_acl_default")
    };
    let expected_acl = acl(mode == "access-acl");
    rustix::fs::setxattr(path, name, &expected_acl, rustix::fs::XattrFlags::CREATE)
        .expect("actual POSIX ACL fixture");
    attribute(path, name, &expected_acl);
    if mode == "default-acl" {
        assert_eq!(
            rustix::fs::listxattr("source", &mut [0u8; 0][..]).expect("source has no access ACL"),
            0
        );
    }
    let before = std::fs::metadata("source").expect("source before refusal");
    let observed = client.call(read("source"));
    let expected = result(&observed)["observations"][0]["revision"]
        .as_str()
        .expect("observed ACL source");
    let reply = client.call(mutation(
        "store",
        json!({"path":"source","contents":"unsafe"}),
        expected,
        "acl-refusal",
    ));
    refused(
        &reply,
        "acl-refusal",
        if mode == "access-acl" {
            "ACLs, xattrs"
        } else {
            "default ACLs"
        },
    );
    assert_eq!(std::fs::read("source").expect("intact source"), original);
    let after = std::fs::metadata("source").expect("intact metadata");
    assert_eq!(
        (after.ino(), after.mode(), after.uid(), after.gid()),
        (before.ino(), before.mode(), before.uid(), before.gid())
    );
    attribute(path, name, &expected_acl);
    if mode == "default-acl" {
        let reply = client.call(mutation(
            "store",
            json!({"path":"absent","contents":"unsafe"}),
            "absent",
            "acl-create-refusal",
        ));
        refused(&reply, "acl-create-refusal", "default ACLs");
        assert!(!std::path::Path::new("absent").exists());
        attribute(path, name, &expected_acl);
    }
    for entry in std::fs::read_dir(".").expect("stage inventory") {
        assert_eq!(entry.expect("directory entry").file_name(), "source");
    }
}

#[test]
fn actual_native_acl_sparse_and_staged_timestamp_policies_hold_on_both_doors_and_backends() {
    for backend in [false, true] {
        for door in ["command", "library"] {
            for case in ["access-acl", "default-acl", "sparse", "timestamps"] {
                let fixture = Fixture::new(backend);
                install(&fixture, TOOLS);
                let mut command = fixture.command_program(
                    true,
                    None,
                    &std::env::current_exe().expect("probe executable"),
                );
                command
                    .args(["--exact", PROBE, "--nocapture"])
                    .env("CASPER_METADATA_BINARY", &fixture.binary)
                    .env("CASPER_METADATA_CASE", case)
                    .env("CASPER_METADATA_DOOR", door);
                let output = completed(Process(command.spawn().expect("actual metadata probe")));
                assert!(String::from_utf8_lossy(&output).contains(&format!("test {PROBE} ... ok")));
            }
        }
    }
}
