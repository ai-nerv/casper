//! Process-isolated resource measurements.

/// Run only `name` in a bounded child, returning true in the parent.
#[cfg(test)]
pub(crate) fn isolated(name: &str) -> bool {
    if std::env::var("CASPER_TEST_PROCESS").is_ok_and(|child| child == name) {
        return false;
    }
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", name, "--nocapture"])
        .env("CASPER_TEST_PROCESS", name)
        .spawn()
        .expect("isolated measurement");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().expect("measurement status") {
            assert!(status.success(), "isolated measurement {name}: {status}");
            return true;
        }
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated measurement {name} exceeded 30 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
