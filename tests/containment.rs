use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "containment/aliases.rs"]
mod aliases;
#[path = "containment/launcher.rs"]
mod launcher;
#[path = "containment/lifecycle.rs"]
mod lifecycle;
#[path = "containment/paths.rs"]
mod paths;
#[path = "containment/socket.rs"]
mod socket;
#[path = "containment/surface.rs"]
mod surface;

fn installed(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn fixture(mode: &str, bubblewrap: bool, variant: &str) {
    let network = std::net::UdpSocket::bind("127.0.0.1:0").expect("host network probe");
    network
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("network deadline");
    let dir = paths::Fixture::new(mode);
    for path in ["home/.ssh", "work", "config", "data/magi", "bin", "tmp"] {
        std::fs::create_dir_all(dir.join(path)).expect("fixture directory");
    }
    for path in [
        "home/.ssh/id",
        "data/magi/credentials.json",
        "home/.external",
    ] {
        std::fs::write(dir.join(path), "SYNTHETIC_SECRET\n").expect("synthetic secret");
    }
    std::os::unix::fs::symlink(dir.join("data/magi"), dir.join("work/alias"))
        .expect("credential alias");
    std::fs::write(dir.join("outside"), "HOST_UNCHANGED").expect("outside fixture");
    aliases::prepare(&dir, variant);
    let local = (variant == "unix-network").then(|| {
        std::fs::copy(
            std::env::current_exe().expect("test executable"),
            dir.join("work/network-probe"),
        )
        .expect("network probe executable");
        std::os::unix::net::UnixListener::bind(dir.join("work/host.sock"))
            .expect("host Unix socket")
    });
    if variant == "toolchain" {
        let compiler = installed("rustc").expect("required Rust toolchain unavailable");
        std::os::unix::fs::symlink(compiler, dir.join("work/rustc")).expect("compiler fixture");
        std::fs::write(
            dir.join("work/fixture.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .expect("compiler input");
    }
    if variant == "replaced-grant" {
        std::fs::create_dir(dir.join("allowed")).expect("original grant");
        std::os::unix::fs::symlink(dir.join("allowed"), dir.join("grant")).expect("grant alias");
    }
    if variant == "replaced-directory" {
        std::fs::create_dir(dir.join("grant")).expect("original grant directory");
    }
    if variant == "descriptor-probe" {
        std::fs::copy(
            std::env::current_exe().expect("test executable"),
            dir.join("work/descriptor-probe"),
        )
        .expect("descriptor probe executable");
    }
    if variant == "default-xdg" {
        std::fs::create_dir_all(dir.join("home/.local/share")).expect("default data");
        std::fs::rename(dir.join("data/magi"), dir.join("home/.local/share/magi"))
            .expect("default store");
        std::fs::remove_file(dir.join("work/alias")).expect("old alias");
        std::os::unix::fs::symlink(dir.join("home/.local/share/magi"), dir.join("work/alias"))
            .expect("default alias");
    } else if matches!(variant, "missing-stores" | "late-stores") {
        for path in ["home/.ssh", "data/magi"] {
            std::fs::remove_dir_all(dir.join(path)).expect("remove owned store");
        }
        std::fs::remove_file(dir.join("home/.external")).expect("remove owned secret");
        std::fs::remove_file(dir.join("work/alias")).expect("remove owned alias");
    }
    if bubblewrap {
        let Some(program) = installed("bwrap") else {
            assert!(
                std::env::var_os("CASPER_REQUIRE_CONTAINMENT").is_none(),
                "required bubblewrap unavailable"
            );
            eprintln!("NOT VERIFIED: bubblewrap unavailable");
            return;
        };
        let program = if matches!(
            variant,
            "unsafe-launcher" | "hardlinked-launcher" | "granted-launcher" | "tmp-launcher"
        ) {
            let source = launcher::unsafe_program(&dir, &program, variant == "hardlinked-launcher");
            if matches!(variant, "granted-launcher" | "tmp-launcher") {
                std::fs::create_dir(dir.join("shared")).expect("shared grant");
                let target = dir.join("shared/launcher");
                std::fs::rename(source, &target).expect("launcher under explicit grant");
                target
            } else {
                source
            }
        } else if variant == "refused" {
            let target = dir.join("bin/refused-backend");
            std::fs::copy("/bin/false", &target).expect("owned failed backend");
            target
        } else {
            program
        };
        std::os::unix::fs::symlink(program, dir.join("bin/bwrap")).expect("bwrap fixture");
    }
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "contained_child", "--nocapture"])
        .current_dir(dir.join("work"))
        .env_clear()
        .env("PATH", dir.join("bin"))
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("TMPDIR", dir.join("tmp"))
        .env("OPENROUTER_API_KEY", "SYNTHETIC_ENV_SECRET")
        .env("BASH_ENV", dir.join("home/.external"))
        .env("CASPER_JAIL", "1")
        .env("CASPER_CONTAINMENT_CHILD", mode)
        .env("CASPER_CONTAINMENT_VARIANT", variant)
        .env("CASPER_FIXTURE_ROOT", &*dir)
        .env(
            "CASPER_PROBE_PORT",
            network.local_addr().expect("listener").port().to_string(),
        )
        .stdin(Stdio::null());
    if variant == "default-xdg" {
        command
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_CONFIG_HOME");
    }
    if variant == "isolation-off" {
        command.env("CASPER_JAIL", "");
    }
    if variant == "broad-grant" {
        command.env(
            "CASPER_JAIL",
            serde_json::json!({ "write": [dir.join("data")], "reach": false }).to_string(),
        );
    }
    if matches!(variant, "replaced-grant" | "replaced-directory") {
        command.env(
            "CASPER_JAIL",
            serde_json::json!({"write":[dir.join("grant")]}).to_string(),
        );
    }
    if variant == "granted-launcher" {
        command.env(
            "CASPER_JAIL",
            serde_json::json!({"write":[dir.join("shared")]}).to_string(),
        );
    }
    if variant == "tmp-launcher" {
        command.env(
            "CASPER_JAIL",
            serde_json::json!({"tmp":dir.join("shared")}).to_string(),
        );
    }
    if variant == "interactive-surface" {
        command.env(
            "CASPER_SURFACE_REPORT",
            std::env::current_dir()
                .expect("repo")
                .join("target/surface")
                .join(if bubblewrap {
                    "bubblewrap.jsonl"
                } else {
                    "landlock.jsonl"
                }),
        );
    }
    if variant == "toolchain" {
        // This machine's toolchain, so the child builds its jail around where the compiler really
        // is rather than around a fixture home that has none.
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }
    let mut child = lifecycle::Owned::new(command.spawn().expect("containment child"));
    if variant == "late-stores" {
        paths::create_after_ready(&mut child, &dir);
    }
    if variant == "descriptor-probe" {
        lifecycle::inspect_descriptors(&mut child, &dir);
    }
    if variant == "parent-death" {
        lifecycle::parent_dies(&mut child, &dir.join("work/ready"));
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("child status") {
            break status;
        }
        if Instant::now() >= deadline {
            child.stop();
            panic!("containment fixture timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "{mode}, bubblewrap={bubblewrap}: {status}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("outside")).expect("outside fixture"),
        "HOST_UNCHANGED"
    );
    let mut packet = [0; 128];
    let error = network
        .recv_from(&mut packet)
        .expect_err("jailed command reached host UDP socket");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    drop(local);
}

fn probe(root: &Path) -> String {
    let port: u16 = std::env::var("CASPER_PROBE_PORT")
        .expect("probe port")
        .parse()
        .expect("port number");
    format!(
        r#"
set -e
report="$PWD/report"
if [ -n "$OPENROUTER_API_KEY$BASH_ENV" ]; then echo ENV_LEAK > "$report"; exit 10; fi
for file in '{root}/home/.ssh/id' '{root}/data/magi/credentials.json' '{root}/home/.local/share/magi/credentials.json' '{root}/home/.external' "$PWD/alias/credentials.json" "$PWD/exposed"; do
  contents=$(/bin/cat "$file" 2>/dev/null || true)
  case "$contents" in *SYNTHETIC_SECRET*) echo READ_LEAK > "$report"; exit 11;; esac
done
if [ -f '{root}/outside' ]; then
  if echo bad > '{root}/outside' 2>/dev/null; then echo WRITE_LEAK > "$report"; exit 12; fi
fi
/bin/bash -c 'echo SYNTHETIC_NETWORK_PACKET > /dev/udp/127.0.0.1/{port}' 2>/dev/null || true
echo workspace > "$PWD/allowed"
temporary=$(/bin/mktemp)
echo temporary > "$temporary"
/bin/rm "$temporary"
echo CONTAINED > "$report"
"#,
        root = root.display()
    )
}

#[test]
fn contained_child() {
    let Ok(mode) = std::env::var("CASPER_CONTAINMENT_CHILD") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("CASPER_FIXTURE_ROOT").expect("fixture root"));
    let variant = std::env::var("CASPER_CONTAINMENT_VARIANT").expect("variant");
    if variant == "interactive-surface" {
        surface::review(&root);
        return;
    }
    if matches!(
        variant.as_str(),
        "unsafe-launcher"
            | "hardlinked-launcher"
            | "replaced-launcher"
            | "granted-launcher"
            | "tmp-launcher"
    ) {
        launcher::check(&root, mode == "screen", variant == "replaced-launcher");
        return;
    }
    if aliases::refused(&variant) {
        aliases::check_refusal(&root, &mode);
        return;
    }
    if matches!(variant.as_str(), "replaced-grant" | "replaced-directory") {
        paths::replaced_grant(&root, mode == "screen", variant == "replaced-directory");
        return;
    }
    if variant == "terminal-io" {
        terminal_io(&root);
        return;
    }
    let mut script = probe(&root);
    if variant == "late-stores" {
        script = format!(
            "echo ready > ready; while [ ! -e release ]; do /bin/sleep 0.01; done;\n{script}"
        );
    }
    if variant == "parent-death" {
        script = "trap '' HUP; echo ready > ready; exec /bin/sleep 600".into();
    }
    if variant == "isolation-off" {
        script = format!(
            "set -e; test -n \"$OPENROUTER_API_KEY\"; test \"$(/bin/cat '{}')\" = SYNTHETIC_SECRET; echo CONTAINED > report",
            root.join("data/magi/credentials.json").display()
        );
    }
    if variant == "toolchain" {
        script.push_str(
            "\n./rustc --crate-type lib --emit=obj fixture.rs -o fixture.o\ntest -s fixture.o\n",
        );
    }
    if variant == "unix-network" {
        script.push_str("\n./network-probe --exact unix_socket_probe --nocapture\n");
    }
    if variant == "descriptor-probe" {
        script.push_str("\n./descriptor-probe --exact descriptor_probe --nocapture\n");
    }
    if variant == "broad-grant" {
        let refused = casper::jail::Jail::from_env().prepare(
            "/bin/sh",
            &["-c".into(), script],
            None,
            &[],
            mode == "screen",
        );
        assert!(
            refused
                .err()
                .expect("overlapping grant must refuse")
                .to_string()
                .contains("credential")
        );
        return;
    }
    if mode == "socket" {
        socket::run(&root, &script, false);
    } else if mode == "command" {
        let done = casper::lua::exec::run("/bin/sh", &["-c".into(), script]);
        if variant == "refused" {
            assert_ne!(done.code, 0, "failed backend must not run unjailed");
            assert!(!root.join("work/report").exists());
            return;
        }
        assert_eq!(
            done.code, 0,
            "command refused or failed: {} {}",
            done.err, done.out
        );
    } else {
        let mut screen = casper::pty::Screen::open(
            &casper::pty::Spec {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), script],
                env: vec![
                    ("OPENROUTER_API_KEY".into(), "SYNTHETIC_INJECTED_KEY".into()),
                    ("HOME".into(), "/".into()),
                    ("CASPER_JAIL".into(), "".into()),
                ],
                ..Default::default()
            },
            24,
            80,
        )
        .expect("jailed screen");
        let deadline =
            Instant::now() + Duration::from_secs(if variant == "parent-death" { 600 } else { 10 });
        while screen.read() {
            assert!(Instant::now() < deadline, "screen did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let report = screen.epitaph();
        screen.close();
        if variant == "refused" {
            assert!(
                !report.contains("status 0"),
                "failed backend must not run unjailed"
            );
            assert!(!root.join("work/report").exists());
            return;
        }
        assert!(
            report.contains("status 0"),
            "screen refused or failed: {report}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("work/report"))
            .expect("probe ran")
            .trim(),
        "CONTAINED"
    );
}

fn terminal_io(root: &Path) {
    let mut screen = casper::pty::Screen::open(
        &casper::pty::Spec {
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "read -r value; /bin/stty size > report; printf '%s\\n' \"$value\" >> report"
                    .into(),
            ],
            ..Default::default()
        },
        24,
        80,
    )
    .expect("jailed interactive screen");
    screen.resized(37, 111);
    screen.typed("x");
    screen.typed("enter");
    let deadline = Instant::now() + Duration::from_secs(10);
    while screen.read() {
        assert!(Instant::now() < deadline, "interactive screen did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let report = screen.epitaph();
    screen.close();
    assert!(report.contains("status 0"), "{report}");
    assert_eq!(
        std::fs::read_to_string(root.join("work/report")).expect("terminal report"),
        "37 111\nx\n"
    );
}

#[test]
fn jailed_screens_preserve_input_resize_and_clean_exit() {
    for bubblewrap in [false, true] {
        fixture("screen", bubblewrap, "terminal-io");
    }
}

#[test]
fn jailed_commands_and_screens_can_compile_workspace_sources() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen"] {
            fixture(mode, bubblewrap, "toolchain");
        }
    }
}

#[test]
fn fallback_commands_exclude_credentials_environment_and_network() {
    fixture("command", false, "override-xdg");
}

#[test]
fn unix_socket_probe() {
    if std::env::current_exe().expect("executable").file_name()
        != Some(std::ffi::OsStr::new("network-probe"))
    {
        return;
    }
    assert!(
        std::os::unix::net::UnixStream::connect("host.sock").is_err(),
        "jailed child reached a host Unix socket"
    );
}

#[test]
fn jailed_commands_and_screens_cannot_reach_host_unix_sockets() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen"] {
            fixture(mode, bubblewrap, "unix-network");
        }
    }
}

#[test]
fn fallback_screens_exclude_credentials_environment_and_network() {
    fixture("screen", false, "override-xdg");
}

#[test]
fn bubblewrap_commands_exclude_credentials_environment_and_network() {
    fixture("command", true, "override-xdg");
}

#[test]
fn bubblewrap_screens_exclude_credentials_environment_and_network() {
    fixture("screen", true, "override-xdg");
}

#[test]
fn prepared_grants_cannot_be_retargeted_to_credentials() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen"] {
            for variant in ["replaced-grant", "replaced-directory"] {
                fixture(mode, bubblewrap, variant);
            }
        }
    }
}

#[test]
fn credentials_created_after_spawn_remain_unreadable() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen", "socket"] {
            fixture(mode, bubblewrap, "late-stores");
        }
    }
}

#[test]
fn descriptor_probe() {
    if std::env::current_exe().expect("executable").file_name()
        != Some(std::ffi::OsStr::new("descriptor-probe"))
    {
        return;
    }
    std::fs::write("probe-ready", "ready").expect("probe ready");
    std::fs::rename("probe-ready", "ready").expect("publish readiness after closing the file");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !Path::new("release").exists() {
        assert!(Instant::now() < deadline, "descriptor inspection timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn jail_setup_descriptors_do_not_reach_commands_or_screens() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen"] {
            fixture(mode, bubblewrap, "descriptor-probe");
        }
    }
}

#[test]
fn default_and_missing_stores_and_broad_grants_are_safe() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen"] {
            for variant in ["default-xdg", "missing-stores", "broad-grant"] {
                fixture(mode, bubblewrap, variant);
            }
        }
    }
}

#[test]
fn actual_interactive_surfaces_preserve_prompt_input_resize_and_exit() {
    for bubblewrap in [false, true] {
        fixture("screen", bubblewrap, "interactive-surface");
    }
}

#[test]
fn writable_or_hardlinked_sandbox_launchers_are_refused() {
    for mode in ["command", "screen"] {
        for variant in [
            "unsafe-launcher",
            "hardlinked-launcher",
            "granted-launcher",
            "tmp-launcher",
        ] {
            fixture(mode, true, variant);
        }
    }
}

#[test]
fn captured_launcher_ignores_a_retargeted_path_symlink() {
    for mode in ["command", "screen"] {
        fixture(mode, true, "replaced-launcher");
    }
}

#[test]
fn hardlinked_credentials_refuse_all_execution_doors() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen", "socket"] {
            for variant in ["hardlinked-credential", "hardlinked-nested"] {
                fixture(mode, bubblewrap, variant);
            }
        }
    }
}

#[test]
fn nested_credential_symlinks_cannot_alias_a_granted_workspace() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen", "socket"] {
            for variant in ["nested-credential", "nested-directory"] {
                fixture(mode, bubblewrap, variant);
            }
        }
    }
}

#[test]
fn safe_nested_credential_targets_and_directory_cycles_remain_usable() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen", "socket"] {
            fixture(mode, bubblewrap, "safe-nested-alias");
        }
    }
}

#[test]
fn socket_commands_keep_the_server_policy() {
    for bubblewrap in [false, true] {
        fixture("socket", bubblewrap, "override-xdg");
    }
}

#[test]
fn jailed_children_die_when_their_parent_is_killed() {
    for bubblewrap in [false, true] {
        for mode in ["command", "screen"] {
            fixture(mode, bubblewrap, "parent-death");
        }
    }
}

#[test]
fn isolation_off_is_explicit_and_failed_backends_do_not_fall_back() {
    for mode in ["command", "screen"] {
        fixture(mode, true, "refused");
        for bubblewrap in [false, true] {
            fixture(mode, bubblewrap, "isolation-off");
        }
    }
}
