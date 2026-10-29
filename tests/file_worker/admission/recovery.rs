use super::*;

#[test]
fn recovery_absence_is_jailed_typed_success_without_text_receipts_and_normal_missing_reads_stay_failed()
 {
    for backend in [false, true] {
        for socket in [false, true] {
            for cbor in [false, true] {
                let fixture = Fixture::new(backend);
                install(&fixture, TOOLS);
                let mut session = Session::new(&fixture, socket);
                let normal = session.call(read("missing"), cbor);
                assert_eq!(normal["ok"], true, "{normal}");
                assert_eq!(
                    normal["result"][0]["failed"], true,
                    "normal missing read changed semantics"
                );
                assert!(normal["result"][0].get("absence").is_none());
                let mut recovery = read("missing");
                recovery["file_recovery"] = json!(1);
                let reply = session.call(recovery.clone(), cbor);
                let resolved = result(&reply);
                assert_ne!(resolved["failed"], true, "{reply}");
                assert_eq!(resolved["absence"]["version"], 1);
                let key = resolved["absence"]["path_key"]
                    .as_str()
                    .expect("opaque binding");
                assert!(
                    key.len() == 64
                        && key
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                );
                assert!(
                    resolved.get("observations").is_none() && resolved.get("mutation").is_none()
                );
                assert!(!fixture.root.join("work/missing").exists());
                std::fs::write(fixture.root.join("work/missing"), "occupied\n")
                    .expect("external creation after proof");
                let refused = session.call(
                    mutation(
                        "store",
                        json!({"path":"missing","contents":"unsafe"}),
                        "absent",
                        "create-after-absence",
                    ),
                    cbor,
                );
                assert_eq!(result(&refused)["mutation"]["outcome"], "refused");
                assert_eq!(
                    std::fs::read_to_string(fixture.root.join("work/missing")).expect("unchanged"),
                    "occupied\n"
                );
                let present = session.call(recovery.clone(), cbor);
                assert!(result(&present).get("absence").is_none());
                assert_eq!(result(&present)["observations"][0]["coverage"], "snapshot");
                std::os::unix::fs::symlink("not-present", fixture.root.join("work/link"))
                    .expect("dangling link");
                for path in [
                    "link",
                    "no-parent/missing",
                    fixture
                        .root
                        .join("home/.ssh/missing")
                        .to_str()
                        .expect("protected path"),
                ] {
                    recovery["args"]["path"] = json!(path);
                    let failed = session.call(recovery.clone(), cbor);
                    assert!(
                        failed["result"][0].get("absence").is_none(),
                        "{path}: {failed}"
                    );
                    assert!(
                        failed["ok"] == false || failed["result"][0]["failed"] == true,
                        "{path}: {failed}"
                    );
                }
            }
        }
    }
}

#[test]
fn recovery_null_unknown_and_mutation_requests_refuse_before_declaration_effects_or_worker_access()
{
    for backend in [false, true] {
        for socket in [false, true] {
            for cbor in [false, true] {
                let fixture = Fixture::new(backend);
                install(
                    &fixture,
                    &format!(
                        "{TOOLS}\ncasper.exec(\"sh\",{{\"-c\",\"printf unsafe > recovery-effect\"}})"
                    ),
                );
                let mut session = Session::new(&fixture, socket);
                for value in [Value::Null, json!(2), json!("1"), json!({})] {
                    let mut call = read("missing");
                    call["file_recovery"] = value;
                    let reply = session.call(call, cbor);
                    assert_eq!(reply["ok"], false, "{reply}");
                }
                let mut write = mutation(
                    "store",
                    json!({"path":"missing","contents":"must not exist"}),
                    "absent",
                    "bad-recovery",
                );
                write["file_recovery"] = json!(1);
                assert_eq!(session.call(write, cbor)["ok"], false);
                assert!(!fixture.root.join("work/recovery-effect").exists());
                assert!(!fixture.root.join("work/missing").exists());
                assert!(!fixture.root.join("var-tmp/.casper-files-v1-0").exists());
            }
        }
    }
}
