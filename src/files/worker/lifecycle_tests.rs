use super::*;
use crate::files::control::ReceivedLease;
use std::os::fd::BorrowedFd;

pub(crate) fn probe(
    path: &str,
    lease: &ReceivedLease<'_>,
    channel: BorrowedFd<'_>,
    mode: &str,
) -> Result<()> {
    let resolved = snapshot::Resolved::open(path)?;
    let request = Request {
        path: path.into(),
        operation: Operation::Replace {
            content: "atomic lifecycle\n".into(),
        },
        guard: Some(Guard {
            revision_contract: 1,
            expected_revision: resolved.capture()?.revision,
            mutation_id: "lifecycle-test".into(),
        }),
    };
    let (target, marker) = match mode {
        "snapshot_verified" => (transaction::Edge::SnapshotVerified, 4),
        "staged" => (transaction::Edge::Staged, 1),
        "before_commit" => (transaction::Edge::BeforeCommit, 2),
        "committed" => (transaction::Edge::Committed, 3),
        "directory_sync" => (transaction::Edge::DirectorySync, 5),
        _ => return Err(Error::other("unknown lifecycle test edge")),
    };
    transaction::run_with(&resolved, lease, &request, |edge| {
        if edge == target {
            let pid = rustix::process::pidfd_open(
                rustix::process::getpid(),
                rustix::process::PidfdFlags::empty(),
            )?;
            control::send_lifecycle_probe(channel, marker, &pid)?;
            control::receive_request(channel)?;
            return Err(Error::other(
                "test worker should have been killed at the barrier",
            ));
        }
        Ok(())
    })?;
    Err(Error::other("lifecycle worker unexpectedly returned"))
}

#[cfg(test)]
pub(crate) fn stage_closure_probe(path: &str) -> Result<()> {
    let resolved = snapshot::Resolved::open(path)?;
    let source = resolved.acquire(true)?;
    let stage = staging::Stage::prepare(&resolved.parent, Some(&source), "exec closure probe\n")?;
    let parent = rustix::fs::fstat(&resolved.parent)?;
    let (device, inode) = stage.identity()?;
    let forbidden = std::env::var("CASPER_NATIVE_TEST_FORBIDDEN").map_err(Error::other)?;
    let forbidden = format!(
        "{forbidden},{device}:{inode},{}:{},{}:{}",
        source.metadata.st_dev, source.metadata.st_ino, parent.st_dev, parent.st_ino
    );
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "jail::native_tests::cases::probe", "--nocapture"])
        .env("CASPER_NATIVE_TEST_ROLE", "fds")
        .env("CASPER_NATIVE_TEST_FORBIDDEN", forbidden)
        .stdout(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(Error::other(
            "native stage/source/parent descriptors survived actual jailed exec",
        ));
    }
    drop(stage);
    Ok(())
}
