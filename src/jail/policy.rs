//! Child environment and protected credential paths.

use super::Grants;
use std::ffi::OsString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

pub(super) fn protected(home: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = [
        ".ssh",
        ".aws",
        ".gnupg",
        ".docker",
        ".kube",
        ".config/gh",
        ".config/gcloud",
        ".external",
        ".cargo/credentials",
        ".cargo/credentials.toml",
    ]
    .iter()
    .map(|path| home.join(path))
    .collect();
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    paths.extend([data.join("magi"), config.join("gh"), config.join("gcloud")]);
    if let Some(cargo) = std::env::var_os("CARGO_HOME") {
        paths.extend([
            PathBuf::from(&cargo).join("credentials"),
            PathBuf::from(cargo).join("credentials.toml"),
        ]);
    }
    let canonical = paths
        .iter()
        .map(|path| resolve(path))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.extend(canonical);
    paths.sort();
    paths.dedup();
    let aliases = super::credentials::expand(&paths)
        .map_err(|error| std::io::Error::other(format!("credential inspection failed: {error}")))?;
    paths.extend(aliases);
    Ok(paths)
}

fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => {
            let name = path
                .file_name()
                .ok_or_else(|| std::io::Error::other("unresolved protected path"))?;
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
                return Err(std::io::Error::other(
                    "dangling protected credential symlink",
                ));
            }
            Ok(resolve(parent)?.join(name))
        }
        Err(why) => Err(why),
    }
}

pub(super) fn validate(cwd: &Path, grants: &Grants, protected: &[PathBuf]) -> std::io::Result<()> {
    let roots = std::iter::once(cwd.to_path_buf())
        .chain(grants.write.iter().cloned())
        .chain(grants.tmp.iter().cloned())
        .chain(super::read_floor());
    for root in roots {
        if !root.exists() {
            continue;
        }
        let root = std::fs::canonicalize(root)?;
        for denied in protected {
            if denied.starts_with(&root) || root.starts_with(denied) {
                return Err(std::io::Error::other(
                    "jail grant overlaps a protected credential path; use a narrower workspace or grant",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validated_fd(path: &Path, protected: &[PathBuf]) -> std::io::Result<OwnedFd> {
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    let actual = super::credentials::opened_path(&fd)?;
    if protected
        .iter()
        .any(|denied| actual.starts_with(denied) || denied.starts_with(&actual))
    {
        return Err(std::io::Error::other(
            "opened jail grant overlaps a protected credential path",
        ));
    }
    Ok(fd)
}

pub(super) fn bindings(
    args: &mut [String],
    protected: &[PathBuf],
    launcher: &Path,
) -> std::io::Result<Vec<OwnedFd>> {
    let mut descriptors = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if matches!(args[index].as_str(), "--bind" | "--ro-bind") {
            let fd = validated_fd(Path::new(&args[index + 1]), protected)?;
            if args[index] == "--bind"
                && launcher.starts_with(super::credentials::opened_path(&fd)?)
            {
                return Err(std::io::Error::other(
                    "sandbox launcher is inside a writable jail grant",
                ));
            }
            args[index].push_str("-fd");
            args[index + 1] = fd.as_raw_fd().to_string();
            descriptors.push(fd);
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(descriptors)
}

pub(super) fn launcher(path: &Path, protected: &[PathBuf]) -> std::io::Result<PathBuf> {
    let fd = validated_fd(path, protected)?;
    let metadata = rustix::fs::fstat(&fd)?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::RegularFile
        || metadata.st_nlink != 1
    {
        return Err(std::io::Error::other(
            "sandbox launcher must be a regular file without hardlink aliases",
        ));
    }
    super::credentials::opened_path(&fd)
}

pub(super) fn environment(
    home: &Path,
    tmp: &Path,
    extra: &[(String, String)],
) -> Vec<(OsString, OsString)> {
    const ALLOWED: &[&str] = &[
        "PATH",
        "TERM",
        "COLORTERM",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "USER",
        "LOGNAME",
        "TZ",
        "LINES",
        "COLUMNS",
        // Where the toolchain is, for a command that builds. The directories themselves are
        // granted read-only and the registry credentials inside them stay protected.
        "CARGO_HOME",
        "RUSTUP_HOME",
    ];
    let mut env: std::collections::BTreeMap<OsString, OsString> = ALLOWED
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
        .collect();
    for (key, value) in extra {
        if ALLOWED.contains(&key.as_str()) {
            env.insert(key.into(), value.into());
        }
    }
    env.insert("HOME".into(), home.into());
    env.insert("TMPDIR".into(), tmp.into());
    env.into_iter().collect()
}

/// Where this machine keeps its Rust toolchain. `CARGO_HOME` itself is never named: it holds the
/// registry credentials, which are protected and would refuse the grant.
pub(super) fn toolchain() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let beneath = |name: &str, fallback: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(fallback)))
    };
    let (cargo, rustup) = (
        beneath("CARGO_HOME", ".cargo"),
        beneath("RUSTUP_HOME", ".rustup"),
    );
    toolchain_in(cargo.as_deref(), rustup.as_deref())
}

/// The half that does not read the environment, so a test can exercise it.
pub(super) fn toolchain_in(cargo: Option<&Path>, rustup: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = rustup.map(PathBuf::from).into_iter().collect();
    if let Some(cargo) = cargo {
        dirs.extend(["bin", "registry", "git"].map(|part| cargo.join(part)));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_rechecks_a_grant_retargeted_after_path_validation() {
        let root = crate::scratch::Scratch::new("grant", "retarget-before-open");
        for name in ["work", "allowed", "secret"] {
            std::fs::create_dir(root.join(name)).expect("fixture directory");
        }
        let link = root.join("grant");
        std::os::unix::fs::symlink(root.join("allowed"), &link).expect("initial grant");
        let protected = vec![root.join("secret")];
        let grants = Grants {
            write: vec![link.clone()],
            ..Grants::default()
        };
        validate(&root.join("work"), &grants, &protected).expect("initial path validation");
        std::fs::remove_file(&link).expect("old link");
        std::os::unix::fs::symlink(root.join("secret"), &link).expect("changed grant");
        let error = validated_fd(&link, &protected).expect_err("opened target must be rejected");
        assert!(error.to_string().contains("credential"));
        assert!(super::super::ruleset(&root.join("work"), &grants, &protected).is_none());
        let mut args = super::super::profile(&root.join("work"), &grants);
        assert!(bindings(&mut args, &protected, Path::new("/usr/bin/bwrap")).is_err());
    }

    #[test]
    fn opened_writable_grants_cannot_cover_the_resolved_launcher() {
        let root = crate::scratch::Scratch::new("launcher", "grants");
        for name in ["work", "allowed", "backend"] {
            std::fs::create_dir(root.join(name)).expect("fixture directory");
        }
        let executable = root.join("backend/bwrap");
        std::fs::write(&executable, "synthetic executable").expect("launcher");
        let link = root.join("grant");
        std::os::unix::fs::symlink(root.join("allowed"), &link).expect("initial grant");
        let grants = Grants {
            write: vec![link.clone()],
            ..Default::default()
        };
        validate(&root.join("work"), &grants, &[]).expect("initial validation");
        let resolved = launcher(&executable, &[]).expect("resolved launcher");
        std::fs::remove_file(&link).expect("old alias");
        std::os::unix::fs::symlink(root.join("backend"), &link).expect("retargeted grant");
        let mut args = super::super::profile(&root.join("work"), &grants);
        assert!(
            bindings(&mut args, &[], &resolved)
                .expect_err("writable launcher")
                .to_string()
                .contains("launcher")
        );
        let mut readonly = vec![
            "--ro-bind".into(),
            link.display().to_string(),
            link.display().to_string(),
        ];
        assert_eq!(
            bindings(&mut readonly, &[], &resolved)
                .expect("readonly launcher mount")
                .len(),
            1
        );
    }

    #[test]
    fn the_toolchain_floor_never_names_the_directory_holding_registry_credentials() {
        let (cargo, rustup) = (Path::new("/c/cargo"), Path::new("/c/rustup"));
        let dirs = toolchain_in(Some(cargo), Some(rustup));
        assert!(!dirs.contains(&cargo.to_path_buf()), "{dirs:?}");
        for part in ["bin", "registry", "git"] {
            assert!(dirs.contains(&cargo.join(part)), "{part}: {dirs:?}");
        }
        assert!(dirs.contains(&rustup.to_path_buf()), "{dirs:?}");
    }

    #[test]
    fn a_machine_without_either_location_asks_for_nothing() {
        assert!(toolchain_in(None, None).is_empty());
    }
}
