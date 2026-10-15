//! Process-isolated resource measurements, and the grammars the suite parses with.

/// Every pinned grammar, built: under `CASPER_GRAMMARS` when it is set, else where
/// `make grammars` builds them.
///
/// # Panics
/// When any is missing, saying how to get them: a test that needs a grammar fails, never skips.
#[cfg(test)]
pub(crate) fn grammars() -> crate::shelf::Shelf {
    let shelf = if std::env::var_os(crate::shelf::NAMED).is_some() {
        crate::shelf::Shelf::then(None)
    } else {
        let built = concat!(env!("CARGO_MANIFEST_DIR"), "/target/grammars");
        crate::shelf::Shelf::at(vec![built.into()])
    };
    let missing: Vec<&str> = crate::grammars::lock()
        .grammars
        .keys()
        .map(String::as_str)
        .filter(|language| shelf.find(language).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "no grammar built for {}: run `oslo make grammars`, or enter `nix develop`, whose \
         CASPER_GRAMMARS names the casper-grammars package",
        missing.join(" ")
    );
    shelf
}

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
