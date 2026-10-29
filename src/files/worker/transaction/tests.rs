#![allow(clippy::expect_used, clippy::indexing_slicing)]

use super::*;
use crate::files::{control, locking::Registry};
use crate::scratch::Scratch;
use rustix::fs::{self, Mode, OFlags};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::sync::atomic::AtomicBool;

struct Fixture {
    root: Scratch,
    registry: Registry,
}

impl Fixture {
    fn new() -> Self {
        let root = Scratch::new("atomic-transactions", "edges");
        for name in ["work", "registry"] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(name))
                .expect("fixture");
        }
        std::fs::write(root.join("work/source"), b"before\r\n").expect("source");
        let fd = fs::open(
            root.join("registry"),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("registry fd");
        Self {
            root,
            registry: Registry::from_private(fd).expect("private registry"),
        }
    }

    fn apply(&self, mut edge: impl FnMut(Edge) -> Result<()>) -> Result<Commit> {
        let path = self.root.join("work/source");
        let resolved = Resolved::open(path.to_str().expect("source path")).expect("binding");
        let held = self
            .registry
            .lease(
                &resolved.binding,
                crate::files::locking::MAX_WAIT,
                &AtomicBool::new(false),
            )
            .expect("lease");
        let (send, receive) = control::pair().expect("handoff");
        held.handoff(send.as_fd()).expect("queue held lease");
        let lease =
            control::receive_lease(receive.as_fd(), &resolved.binding).expect("worker-owned lease");
        drop(held);
        let request = Request {
            path: path.display().to_string(),
            operation: Operation::Replace {
                content: "after\r\n".into(),
            },
            guard: Some(super::super::protocol::Guard {
                revision_contract: 1,
                expected_revision: resolved.capture().expect("observed source").revision,
                mutation_id: "edge-test".into(),
            }),
        };
        run_with(&resolved, &lease, &request, &mut edge)
    }

    fn entries(&self, directory: &str) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(self.root.join(directory))
            .expect("source directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect()
    }
}

#[test]
fn every_precommit_edge_failure_keeps_the_target_and_cleans_owned_stages() {
    for target in [Edge::SnapshotVerified, Edge::Staged, Edge::BeforeCommit] {
        let fixture = Fixture::new();
        let before = std::fs::metadata(fixture.root.join("work/source")).expect("before");
        let error = fixture
            .apply(|edge| {
                if edge == target {
                    Err(Error::other("injected precommit failure"))
                } else {
                    Ok(())
                }
            })
            .expect_err("edge must refuse");
        assert!(error.to_string().contains("injected"));
        assert_eq!(
            std::fs::read(fixture.root.join("work/source")).expect("target intact"),
            b"before\r\n"
        );
        assert_eq!(
            std::fs::metadata(fixture.root.join("work/source"))
                .expect("same inode")
                .ino(),
            before.ino()
        );
        assert_eq!(
            fixture.entries("work"),
            vec![std::ffi::OsString::from("source")]
        );
    }
}

#[test]
fn mutation_during_staging_or_parent_rename_refuses_before_commit() {
    for rename_parent in [false, true] {
        let fixture = Fixture::new();
        let result = fixture.apply(|edge| {
            if edge == Edge::BeforeCommit {
                if rename_parent {
                    std::fs::rename(fixture.root.join("work"), fixture.root.join("moved"))?;
                    std::fs::create_dir(fixture.root.join("work"))?;
                } else {
                    std::fs::write(fixture.root.join("work/source"), b"external\n")?;
                }
            }
            Ok(())
        });
        let error = result.expect_err("binding/content drift must refuse");
        assert!(
            error.to_string().contains(if rename_parent {
                "parent binding changed"
            } else {
                "changed before commit"
            }),
            "{error}"
        );
        let directory = if rename_parent { "moved" } else { "work" };
        assert_eq!(
            fixture.entries(directory),
            vec![std::ffi::OsString::from("source")]
        );
        assert_eq!(
            std::fs::read(fixture.root.join(directory).join("source")).expect("no commit"),
            if rename_parent {
                b"before\r\n".as_slice()
            } else {
                b"external\n".as_slice()
            }
        );
    }
}

#[test]
fn postcommit_sync_and_reporting_failures_never_claim_rollback_or_refusal() {
    for target in [Edge::Committed, Edge::DirectorySync] {
        let fixture = Fixture::new();
        let result = fixture
            .apply(|edge| {
                if edge == target {
                    Err(Error::other("injected postcommit failure"))
                } else {
                    Ok(())
                }
            })
            .expect("commit remains committed");
        assert_eq!(result.mutation.id, "edge-test");
        assert_eq!(result.mutation.outcome, "committed");
        assert_eq!(
            result.mutation.durability,
            if target == Edge::DirectorySync {
                "unconfirmed"
            } else {
                "synced"
            }
        );
        assert!(
            result
                .warning
                .expect("explicit warning")
                .contains("committed")
        );
        assert_eq!(
            result
                .observation
                .expect("current delivered candidate")
                .source_text,
            "after\r\n"
        );
        assert_eq!(
            std::fs::read(fixture.root.join("work/source")).expect("committed target"),
            b"after\r\n"
        );
        assert_eq!(
            fixture.entries("work"),
            vec![std::ffi::OsString::from("source")]
        );
    }
}

#[test]
fn a_replaced_stage_name_is_neither_committed_nor_deleted() {
    let fixture = Fixture::new();
    let mut foreign = None;
    let result = fixture.apply(|edge| {
        if edge == Edge::BeforeCommit {
            let name = fixture
                .entries("work")
                .into_iter()
                .find(|name| name.as_encoded_bytes().starts_with(b".casper-stage-"))
                .expect("named native stage");
            let path = fixture.root.join("work").join(name);
            std::fs::rename(&path, fixture.root.join("work/moved-stage"))?;
            std::fs::write(&path, b"FOREIGN INODE")?;
            foreign = Some(path);
        }
        Ok(())
    });
    assert!(
        result
            .expect_err("stage binding drift must refuse")
            .to_string()
            .contains("stage binding changed")
    );
    assert_eq!(
        std::fs::read(foreign.expect("foreign stage path")).expect("must not delete foreign stage"),
        b"FOREIGN INODE"
    );
    assert_eq!(
        std::fs::read(fixture.root.join("work/source")).expect("target intact"),
        b"before\r\n"
    );
}

#[test]
fn committed_envelopes_require_identity_supported_version_and_explicit_uncertainty() {
    let fixture = Fixture::new();
    let result = fixture.apply(|_| Ok(())).expect("committed producer");
    let key =
        Key::from_wire(&result.observation.as_ref().expect("observation").path_key).expect("key");
    let request = Request {
        path: "source".into(),
        operation: Operation::Replace {
            content: "after\r\n".into(),
        },
        guard: Some(super::super::protocol::Guard {
            revision_contract: 1,
            expected_revision: format!("cfr1:{}", "0".repeat(64)),
            mutation_id: "edge-test".into(),
        }),
    };
    let original = serde_json::json!({"revision_contract":1,"mutation":{"id":"edge-test","outcome":"committed","durability":"synced"},
        "observation":serde_json::to_value(result.observation).expect("producer observation"),"warning":null});
    serde_json::from_value::<Commit>(original.clone())
        .expect("independent golden")
        .validate(&key, &request)
        .expect("accepted contract");
    for patch in [
        serde_json::json!({"revision_contract":2}),
        serde_json::json!({"mutation":{"id":"other","outcome":"committed","durability":"synced"}}),
        serde_json::json!({"mutation":{"id":"edge-test","outcome":"refused","durability":"synced"}}),
        serde_json::json!({"mutation":{"id":"edge-test","outcome":"committed","durability":"unconfirmed"}}),
        serde_json::json!({"observation":null}),
    ] {
        let mut changed = original.clone();
        for (key, value) in patch.as_object().expect("patch") {
            changed[key] = value.clone();
        }
        assert!(
            serde_json::from_value::<Commit>(changed)
                .expect("typed corrupted reply")
                .validate(&key, &request)
                .is_err()
        );
    }
    let unknown = Failure::Unknown(Error::other("ack lost after dispatch"));
    assert!(unknown.outcome_unknown());
    assert!(unknown.to_string().contains("do not replay"));
    assert!(!Failure::Refused(Error::other("stale before staging")).outcome_unknown());
}
