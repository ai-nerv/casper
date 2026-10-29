#![allow(clippy::unwrap_used)]

use super::*;
fn shell(script: &str) -> crate::jail::Prepared {
    crate::jail::workflow_test_shell(script)
}

#[test]
fn exchange_handles_partial_writes_and_exact_output() {
    let input = vec![b'x'; super::super::protocol::INPUT_MOST];
    assert_eq!(
        exchange(
            shell("cat"),
            &input,
            &AtomicBool::new(false),
            Duration::from_secs(3)
        )
        .unwrap(),
        input
    );
}

#[test]
fn exchange_bounds_each_pipe_and_rejects_failed_exit() {
    for script in ["head -c 7340033 /dev/zero", "head -c 4097 /dev/zero >&2"] {
        assert_eq!(
            exchange(
                shell(script),
                b"",
                &AtomicBool::new(false),
                Duration::from_secs(3)
            ),
            Err(Failure::Protocol)
        );
    }
    assert_eq!(
        exchange(
            shell("head -c 4096 /dev/zero >&2; printf ok"),
            b"",
            &AtomicBool::new(false),
            Duration::from_secs(3)
        ),
        Ok(b"ok".to_vec())
    );
    assert_eq!(
        exchange(
            shell("exit 7"),
            b"",
            &AtomicBool::new(false),
            Duration::from_secs(3)
        ),
        Err(Failure::Worker)
    );
}

#[test]
fn cancelled_or_oversized_input_never_spawns() {
    let fixture = crate::scratch::Scratch::new("workflow", "no-spawn");
    let command = || {
        let mut c = shell("touch \"$MARKER\"");
        c.inner(&[(
            "MARKER".into(),
            fixture.join("started").to_str().unwrap().into(),
        )]);
        c
    };
    assert_eq!(
        exchange(
            command(),
            b"",
            &AtomicBool::new(true),
            Duration::from_secs(3)
        ),
        Err(Failure::Cancelled)
    );
    assert_eq!(
        exchange(
            command(),
            &vec![0; super::super::protocol::INPUT_MOST + 1],
            &AtomicBool::new(false),
            Duration::from_secs(3)
        ),
        Err(Failure::Protocol)
    );
    assert_eq!(
        exchange(command(), b"", &AtomicBool::new(false), Duration::ZERO),
        Err(Failure::Timeout)
    );
    assert!(!fixture.join("started").exists());
}

fn assert_reaped(path: &std::path::Path) {
    let pid: i32 = std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    assert!(matches!(
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn timeout_reaps_stalled_input_and_eof_without_exit() {
    for script in [
        "printf '%s' $$ > \"$PID\"; exec sleep 30",
        "printf '%s' $$ > \"$PID\"; exec 1>&- 2>&-; exec sleep 30",
    ] {
        let fixture = crate::scratch::Scratch::new("workflow", "timeout");
        let pid = fixture.join("pid");
        let mut command = shell(script);
        command.inner(&[("PID".into(), pid.to_str().unwrap().into())]);
        let started = Instant::now();
        assert_eq!(
            exchange(
                command,
                &vec![b'x'; 32768],
                &AtomicBool::new(false),
                Duration::from_secs(1)
            ),
            Err(Failure::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_reaped(&pid);
    }
}

#[test]
fn cancellation_reaps_running_worker() {
    let fixture = crate::scratch::Scratch::new("workflow", "cancel");
    let pid = fixture.join("pid");
    let mut command = shell("printf '%s' $$ > \"$PID\"; exec sleep 30");
    command.inner(&[("PID".into(), pid.to_str().unwrap().into())]);
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !std::fs::read_to_string(&pid).is_ok_and(|text| text.parse::<u32>().is_ok())
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            cancel.store(true, Ordering::Relaxed);
        });
        assert_eq!(
            exchange(command, b"", &cancel, Duration::from_secs(3)),
            Err(Failure::Cancelled)
        );
    });
    assert_reaped(&pid);
}

#[test]
fn exited_leader_with_inherited_pipe_is_not_reaped_before_cleanup() {
    let fixture = crate::scratch::Scratch::new("workflow", "descendant");
    let mut command =
        shell("printf '%s' $$ > \"$PID\"; sleep 30 & echo $! > \"$DESCENDANT\"; exit 0");
    command.inner(&[
        ("PID".into(), fixture.join("pid").to_str().unwrap().into()),
        (
            "DESCENDANT".into(),
            fixture.join("descendant").to_str().unwrap().into(),
        ),
    ]);
    assert_eq!(
        exchange(
            command,
            b"",
            &AtomicBool::new(false),
            Duration::from_secs(1)
        ),
        Err(Failure::Timeout)
    );
    assert_reaped(&fixture.join("pid"));
    let pid = std::fs::read_to_string(fixture.join("descendant")).unwrap();
    let stat = format!("/proc/{}/stat", pid.trim());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::read_to_string(&stat) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Ok(text)
                if text
                    .split_once(") ")
                    .is_some_and(|(_, tail)| tail.starts_with('Z')) =>
            {
                break;
            }
            _ => assert!(Instant::now() < deadline, "descendant still running"),
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
