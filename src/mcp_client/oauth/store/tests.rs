#[test]
fn owner_release_unlocks_record_and_refresh_even_with_a_duplicated_descriptor() {
    use super::*;
    for refresh in [false, true] {
        let root = crate::scratch::Scratch::new("oauth", "lock-alias");
        let config: super::super::Config = serde_json::from_str(r#"{"issuer":"https://issuer.example","resource":"https://resource.example/mcp","client_id":"lock-owner"}"#).expect("config");
        let store = Store::at(
            &root.join("store"),
            config.bind(&config.resource).expect("binding"),
        )
        .expect("store");
        let held = if refresh {
            store.try_refresh().expect("refresh").expect("available")
        } else {
            store.locked().expect("record")
        };
        let alias = held.try_clone().expect("shared open file description");
        let suffix = if refresh { "refresh.lock" } else { "lock" };
        let path = root.join("store").join(format!("{}.{suffix}", store.key));
        let contender = File::open(&path).expect("independent contender");
        assert_eq!(
            fs::flock(&contender, FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK)
        );
        drop(held);
        fs::flock(&contender, FlockOperation::NonBlockingLockExclusive)
            .expect("owner unlocks before inherited alias closes");
        drop(alias);
        let another = File::open(path).expect("third contender");
        assert_eq!(
            fs::flock(&another, FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK),
            "closing old alias cannot unlock the successor"
        );
    }
}

#[cfg(test)]
mod native {
    use super::super::*;
    use crate::scratch::Scratch;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn cross_process_worker() {
        let Some(root) = std::env::var_os("CASPER_OAUTH_STORE_PROBE") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let store = Store::at(&root.join("store"), binding()).expect("store");
        std::fs::write(root.join(format!("ready-{}", std::process::id())), b"ready")
            .expect("ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.join("go").exists() {
            assert!(Instant::now() < deadline, "release deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        let outcome = match store.replace(1, credential("process-rotation")) {
            Ok(2) => "committed",
            Err(Error::Changed) => "stale",
            other => panic!("unexpected {other:?}"),
        };
        std::fs::write(root.join(format!("result-{}", std::process::id())), outcome)
            .expect("result");
    }

    #[test]
    fn independent_processes_share_rotation_lock_and_generation_fence() {
        let root = Scratch::new("oauth", "process-lock");
        Store::at(&root.join("store"), binding())
            .expect("store")
            .replace(0, credential("original"))
            .expect("seed");
        let executable = std::env::current_exe().expect("test executable");
        let mut children: Vec<_> = (0..4)
            .map(|_| {
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "mcp_client::oauth::store::tests::native::cross_process_worker",
                        "--nocapture",
                    ])
                    .env("CASPER_OAUTH_STORE_PROBE", &*root)
                    .spawn()
                    .expect("worker")
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !children
            .iter()
            .all(|child| root.join(format!("ready-{}", child.id())).is_file())
        {
            if Instant::now() >= deadline {
                for child in &mut children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                panic!("worker readiness deadline");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::write(root.join("go"), b"release").expect("release");
        let mut committed = 0;
        for child in &mut children {
            loop {
                if let Some(status) = child.try_wait().expect("poll") {
                    assert!(status.success());
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("worker completion deadline");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let result = std::fs::read_to_string(root.join(format!("result-{}", child.id())))
                .expect("outcome");
            if result == "committed" {
                committed += 1;
            } else {
                assert_eq!(result, "stale");
            }
        }
        assert_eq!(committed, 1);
        let store = Store::at(&root.join("store"), binding()).expect("reopen");
        assert_eq!(store.snapshot().expect("state").generation, 2);
        assert_eq!(store.logout().expect("logout"), 3);
        assert_eq!(
            store.replace(2, credential("late-process-token")),
            Err(Error::Changed)
        );
    }

    fn binding() -> Binding {
        let config: crate::mcp_client::oauth::Config = serde_json::from_str(r#"{"issuer":"https://issuer.example","resource":"https://resource.example/mcp","client_id":"public-native"}"#).expect("config");
        config
            .bind("https://resource.example/mcp")
            .expect("binding")
    }

    #[test]
    fn legacy_records_gain_an_epoch_and_short_tokens_remain_usable_until_expiry() {
        let root = Scratch::new("oauth", "legacy");
        let store = Store::at(&root.join("store"), binding()).expect("store");
        store.replace(0, credential("legacy")).expect("save");
        let path = root.join("store").join(format!("{}.json", binding().key()));
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
        record["schema"] = serde_json::json!(1);
        record.as_object_mut().expect("record").remove("epoch");
        record
            .as_object_mut()
            .expect("record")
            .remove("refresh_after");
        std::fs::write(&path, serde_json::to_vec(&record).expect("encode")).expect("legacy");
        let legacy = store.snapshot().expect("legacy state");
        assert_eq!(legacy.epoch, legacy.generation);
        assert_eq!(store.logout().expect("logout"), 2);
        assert_eq!(store.snapshot().expect("tombstone").epoch, 2);
        let mut short = credential("short");
        short.expires_at = Some(1002);
        assert!(short.fresh(1001));
        assert!(!short.fresh(1002));
        short.expires_at = None;
        assert!(short.fresh(1002));
    }

    fn credential(value: &str) -> Credential {
        Credential {
            access_token: value.into(),
            refresh_token: Some("refresh-secret-canary".into()),
            expires_at: Some(10000),
            scopes: vec!["read".into()],
        }
    }

    #[test]
    fn atomic_credentials_are_private_and_logout_fences_stale_results() {
        let root = Scratch::new("oauth", "generation");
        let store = Store::at(&root.join("store"), binding()).expect("store");
        assert_eq!(store.snapshot().expect("missing").generation, 0);
        assert_eq!(
            store
                .replace(0, credential("first-secret-canary"))
                .expect("save"),
            1
        );
        let first = store.snapshot().expect("saved");
        assert_eq!(
            first.credential.as_ref().expect("credential").access_token,
            "first-secret-canary"
        );
        assert!(!format!("{first:?}").contains("secret-canary"));
        assert_eq!(
            store.replace(0, credential("late-secret-canary")),
            Err(Error::Changed)
        );
        assert_eq!(store.logout().expect("logout"), 2);
        assert_eq!(
            store.replace(1, credential("resurrected-secret-canary")),
            Err(Error::Changed)
        );
        let logged_out = Store::at(&root.join("store"), binding())
            .expect("reopen")
            .snapshot()
            .expect("state");
        assert_eq!(logged_out.generation, 2);
        assert!(logged_out.credential.is_none());
        let disk =
            std::fs::read_to_string(root.join("store").join(format!("{}.json", binding().key())))
                .expect("record");
        assert!(!disk.contains("secret-canary"));
        for entry in std::fs::read_dir(root.join("store")).expect("directory") {
            let path = entry.expect("entry").path();
            assert_eq!(
                std::fs::metadata(path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o7777,
                0o600
            );
        }
        assert_eq!(
            store
                .replace(2, credential("new-secret-canary"))
                .expect("new explicit login"),
            3
        );
    }

    #[test]
    fn concurrent_rotations_cannot_overwrite_newer_material() {
        let root = Scratch::new("oauth", "concurrent");
        let path = root.join("store");
        Store::at(&path, binding())
            .expect("store")
            .replace(0, credential("original"))
            .expect("first");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let tasks: Vec<_> = (0..8)
            .map(|n| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let store = Store::at(&path, binding()).expect("store");
                    barrier.wait();
                    store.replace(1, credential(&format!("rotation-{n}")))
                })
            })
            .collect();
        barrier.wait();
        let mut committed = 0;
        for task in tasks {
            match task.join().expect("thread") {
                Ok(2) => committed += 1,
                Err(Error::Changed) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(committed, 1);
        assert_eq!(
            Store::at(&path, binding())
                .expect("reopen")
                .snapshot()
                .expect("state")
                .generation,
            2
        );
    }

    #[test]
    fn symlink_hardlink_mode_and_replaced_directory_are_refused() {
        let root = Scratch::new("oauth", "unsafe");
        let directory = root.join("store");
        let store = Store::at(&directory, binding()).expect("store");
        store.replace(0, credential("secret-canary")).expect("save");
        let record = directory.join(format!("{}.json", binding().key()));
        let alias = root.join("alias");
        std::fs::hard_link(&record, &alias).expect("hardlink");
        assert!(matches!(store.snapshot(), Err(Error::Store)));
        assert_eq!(store.logout(), Err(Error::Store));
        std::fs::remove_file(&alias).expect("remove alias");
        std::fs::rename(&record, &alias).expect("move record");
        symlink(&alias, &record).expect("symlink");
        assert!(matches!(store.snapshot(), Err(Error::Store)));
        std::fs::remove_file(&record).expect("remove link");
        std::fs::rename(&alias, &record).expect("restore");
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o640))
            .expect("insecure");
        assert!(matches!(store.snapshot(), Err(Error::Store)));
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).expect("private");
        std::fs::rename(&directory, root.join("moved")).expect("move directory");
        std::fs::create_dir(&directory).expect("replace directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("private");
        assert!(matches!(store.snapshot(), Err(Error::Store)));
        symlink(root.join("moved"), root.join("linked")).expect("directory symlink");
        assert!(Store::at(&root.join("linked/nested"), binding()).is_err());
    }

    #[test]
    fn invalid_bound_records_and_credential_values_fail_without_secret_errors() {
        let root = Scratch::new("oauth", "invalid");
        let store = Store::at(&root.join("store"), binding()).expect("store");
        for token in [
            String::new(),
            "header\r\ninjection".into(),
            "☃".into(),
            "a".repeat(16385),
        ] {
            assert_eq!(store.replace(0, credential(&token)), Err(Error::Credential));
        }
        store.replace(0, credential("secret-canary")).expect("save");
        let path = root.join("store").join(format!("{}.json", binding().key()));
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("record");
        for field in ["schema", "binding", "credential"] {
            let mut value = original.clone();
            match field {
                "schema" => value["schema"] = serde_json::json!(3),
                "binding" => value["binding"]["client_id"] = serde_json::json!("other"),
                _ => value["credential"]["unexpected"] = serde_json::json!("secret-canary"),
            }
            std::fs::write(&path, serde_json::to_vec(&value).expect("encode")).expect("write");
            let error = store.snapshot().expect_err("invalid record");
            assert_eq!(error, Error::Store);
            assert!(!error.to_string().contains("secret-canary"));
        }
        std::fs::write(&path, vec![b'x'; LIMIT as usize + 1]).expect("oversized");
        assert!(matches!(store.snapshot(), Err(Error::Store)));
    }

    #[test]
    fn lock_contention_is_bounded_and_generation_exhaustion_is_not_wrapped() {
        let root = Scratch::new("oauth", "lock");
        let store = Store::at(&root.join("store"), binding()).expect("store");
        let held = store.locked().expect("held");
        let began = Instant::now();
        assert!(matches!(store.snapshot(), Err(Error::Busy)));
        assert!(began.elapsed() >= WAIT && began.elapsed() < Duration::from_secs(5));
        drop(held);
        store.replace(0, credential("token")).expect("save");
        let path = root.join("store").join(format!("{}.json", binding().key()));
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("record");
        record["generation"] = serde_json::json!(u64::MAX);
        std::fs::write(&path, serde_json::to_vec(&record).expect("encode")).expect("write");
        assert_eq!(store.logout(), Err(Error::Store));
        assert_eq!(
            store.snapshot().expect("state retained").generation,
            u64::MAX
        );
    }
}
