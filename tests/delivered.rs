//! What casper's exit code says about whether the caller got the reply.
//!
//! Against the real binary, because the thing under test is a buffered `stdout` and a process's
//! exit code, neither of which exists inside a unit test. `/dev/full` is the fixture: it accepts
//! an `open`, reports `ENOSPC` on every write, and is on every Linux machine — so a reply that
//! could not be delivered is arranged without asking anything of the run's timing.
//!
//! A pipe would be the obvious fixture and is the wrong one. `casper verbs | true` writes about
//! a kilobyte into a 64 KiB pipe buffer and succeeds whether or not anybody is ever going to
//! read it, so it reports delivery on a reply nothing received — it passes against both the
//! working version and the broken one.

use std::process::{Command, Stdio};

/// Run one verb with stdout on `/dev/full`, and say whether casper called it delivered.
fn delivered_to_a_full_device(args: &[&str]) -> bool {
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full");
    Command::new(env!("CARGO_BIN_EXE_casper"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(full)
        .stderr(Stdio::null())
        .status()
        .expect("casper runs")
        .success()
}

#[test]
fn a_json_reply_that_did_not_land_is_not_a_success() {
    assert!(!delivered_to_a_full_device(&["verbs"]));
}

#[test]
fn a_cbor_reply_that_did_not_land_is_not_a_success_either() {
    // The one that was still passing after the write was checked. CBOR carries no trailing
    // newline and a reply is small, so `stdout`'s `LineWriter` holds it and `write_all` returns
    // `Ok`; what went wrong appears in the flush that `Drop` runs and ignores. Without the
    // explicit flush in `say` this exits 0 having delivered nothing — on the encoding a harness
    // is the one to ask for.
    assert!(!delivered_to_a_full_device(&["--cbor", "verbs"]));
}

#[test]
fn a_reply_that_landed_is_a_success() {
    // The control. Without it the two above pass just as well against a casper that always
    // fails, which would be a worse program and a green suite.
    let out = Command::new(env!("CARGO_BIN_EXE_casper"))
        .arg("verbs")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("casper runs");
    assert!(out.status.success(), "a reply nobody interfered with");
    assert!(!out.stdout.is_empty(), "and there was one");
}

#[test]
fn a_refusal_still_exits_zero() {
    // The family's rule, and the line this change had to not cross: a refusal is an answer, so
    // it leaves by the front door. Only a reply that never arrived is a failure.
    let out = Command::new(env!("CARGO_BIN_EXE_casper"))
        .arg("no-such-verb")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("casper runs");
    assert!(out.status.success(), "a refusal is not a failure");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"ok\":false"),
        "and it is still a refusal"
    );
}
