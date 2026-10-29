//! Native source replacement in the worker's filesystem namespace.

use std::io::{Error, ErrorKind, Write};

pub(crate) const EDIT_BYTES: usize = 1024 * 1024;

pub(crate) fn replace(path: &str, text: &str) -> Result<usize, Error> {
    if crate::jail::Jail::from_env().on() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "source worker requires a prepared namespace",
        ));
    }
    if text.len() >= EDIT_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "editable text must be smaller than 1 MiB",
        ));
    }
    if text.as_bytes().contains(&0) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "editable text must not contain NUL bytes",
        ));
    }
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)?;
    Ok(text.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_configured_jail_cannot_be_bypassed_by_calling_the_write_worker() {
        if let Some(path) = std::env::var_os("CASPER_SOURCE_WORKER_PROBE") {
            let path = path.to_str().expect("fixture path");
            assert!(matches!(
                super::replace(path, "new"),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied
            ));
            assert_eq!(std::fs::read(path).expect("unchanged source"), b"old");
            return;
        }
        let dir = crate::scratch::Scratch::new("source", "worker-policy");
        let path = dir.join("source.txt");
        std::fs::write(&path, "old").expect("source fixture");
        let status = std::process::Command::new("timeout")
            .args(["--kill-after=1s", "5s"]).arg(std::env::current_exe().expect("test executable"))
            .args(["--exact", "source_worker::tests::a_configured_jail_cannot_be_bypassed_by_calling_the_write_worker"])
            .env("CASPER_JAIL", "1").env("CASPER_SOURCE_WORKER_PROBE", &path)
            .stdout(std::process::Stdio::null())
            .status().expect("worker policy probe");
        assert!(status.success());
    }
}
