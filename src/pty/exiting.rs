use super::*;
use std::time::{Duration, Instant};

#[cfg(test)]
#[test]
fn a_program_that_closes_its_pty_is_not_done_before_its_exit() {
    let dir = crate::scratch::Scratch::new("casper-pty", "exit-barrier");
    let release = dir.join("release");
    let mut screen = Screen::open(&Spec {
        command:"/bin/sh".into(),
        args:vec!["-c".into(), "exec </dev/null >/dev/null 2>&1; while [ ! -e \"$1\" ]; do sleep 0.01; done; exit 7".into(), "fixture".into(), release.display().to_string()],
        cwd:Some(dir.display().to_string()),
        ..Default::default()
    }, 3, 20).expect("screen");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(
            screen.output.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "PTY reader did not close");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(screen.child.try_wait().expect("child status").is_none());
    assert!(screen.read(), "closed output is not an exited child");
    std::fs::write(release, "go").expect("release child");
    while screen.read() {
        assert!(Instant::now() < deadline, "child did not exit");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(screen.epitaph().contains("status 7"));
}
