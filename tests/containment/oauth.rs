use std::{path::Path, process::Command};

pub(super) fn seed(root: &Path) {
    std::fs::create_dir_all(root.join("state/casper/mcp-auth")).expect("OAuth store");
    std::fs::write(
        root.join("state/casper/mcp-auth/credential.json"),
        "SYNTHETIC_SECRET\n",
    )
    .expect("OAuth credential");
    std::os::unix::fs::symlink(
        root.join("state/casper/mcp-auth"),
        root.join("work/auth-alias"),
    )
    .expect("OAuth alias");
}

pub(super) fn layout(root: &Path, variant: &str) {
    match variant {
        "oauth-workspace" => {
            std::fs::rename(root.join("state"), root.join("work/state")).expect("workspace state");
            std::fs::remove_file(root.join("work/auth-alias")).expect("old OAuth alias");
        }
        "default-xdg" => {
            std::fs::create_dir_all(root.join("home/.local/state")).expect("default state");
            std::fs::rename(
                root.join("state/casper"),
                root.join("home/.local/state/casper"),
            )
            .expect("default OAuth store");
            std::fs::remove_file(root.join("work/auth-alias")).expect("old OAuth alias");
            std::os::unix::fs::symlink(
                root.join("home/.local/state/casper/mcp-auth"),
                root.join("work/auth-alias"),
            )
            .expect("default OAuth alias");
        }
        "missing-stores" | "late-stores" => {
            std::fs::remove_dir_all(root.join("state/casper/mcp-auth"))
                .expect("remove OAuth store");
            std::fs::remove_file(root.join("work/auth-alias")).expect("remove OAuth alias");
        }
        _ => {}
    }
}

pub(super) fn command(command: &mut Command, root: &Path, variant: &str) {
    match variant {
        "oauth-grant" => {
            command.env(
                "CASPER_JAIL",
                serde_json::json!({"write":[root.join("state")]}).to_string(),
            );
        }
        "oauth-workspace" => {
            command.env("XDG_STATE_HOME", root.join("work/state"));
        }
        _ => {}
    }
}

#[test]
fn credential_roots_and_hardlink_aliases_cannot_be_granted_to_any_execution_door() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen", "socket"] {
            for variant in ["oauth-grant", "oauth-workspace", "oauth-hardlink"] {
                super::fixture(mode, bubblewrap, variant);
            }
        }
    }
}
