use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct NativeNamespace {
    root: PathBuf,
    workspace: PathBuf,
    launcher: PathBuf,
}

impl NativeNamespace {
    pub fn new(root: &Path, workspace: &Path) -> Self {
        assert!(
            !root.starts_with("/var/tmp"),
            "fixture root must survive the private registry mount"
        );
        for name in [
            "home", "config", "data", "state", "cache", "tmp", "runtime", "var-tmp",
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(&path).expect("private fixture directory");
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(if name == "var-tmp" { 0o1777 } else { 0o700 }),
            )
            .expect("fixture directory mode");
        }
        let launcher = std::env::split_paths(&std::env::var_os("PATH").expect("fixture PATH"))
            .map(|path| path.join("bwrap"))
            .find(|path| path.is_file())
            .expect("captured bubblewrap prerequisite");
        let metadata = std::fs::metadata(&launcher).expect("backend metadata");
        assert_eq!(
            metadata.mode() & 0o222,
            0,
            "recipe must capture a read-only backend"
        );
        assert_eq!(
            metadata.nlink(),
            1,
            "backend must not have hardlink aliases"
        );
        Self {
            root: root.to_owned(),
            workspace: workspace.to_owned(),
            launcher,
        }
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(&self.launcher);
        command
            .args([
                "--unshare-user",
                "--uid",
                "0",
                "--gid",
                "0",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--cap-add",
                "ALL",
            ])
            .arg("--bind")
            .arg(&self.root)
            .arg(&self.root)
            .arg("--bind")
            .arg(&self.workspace)
            .arg(&self.workspace)
            .arg("--bind")
            .arg(self.root.join("var-tmp"))
            .arg("/var/tmp")
            .args(["--die-with-parent", "--new-session"])
            .arg("--")
            .arg(program)
            .current_dir(&self.workspace)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("fixture PATH"))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("CASPER_JAIL", "1");
        command
    }
}
