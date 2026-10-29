use super::Fixture;
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

impl Fixture {
    fn mutate(&self, request: &Value) -> Value {
        self.mutate_mask(request, None)
    }

    fn mutate_mask(&self, request: &Value, mask: Option<&str>) -> Value {
        self.run_input(
            self.command_mask(true, mask),
            &["code", "--file-transaction"],
            Some(serde_json::to_vec(request).expect("complete JSON request")),
        )
    }

    fn clean_stages(&self) {
        for entry in std::fs::read_dir(self.root.join("work")).expect("source directory") {
            assert!(
                !entry
                    .expect("entry")
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b".casper-stage-"),
                "native stage left after completed request"
            );
        }
    }
}

fn request(path: &str, operation: Value, expected: &str, id: &str) -> Value {
    json!({"path":path,"operation":operation,
        "guard":{"revision_contract":1,"expected_revision":expected,"mutation_id":id}})
}

fn revision(reply: &Value) -> &str {
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"][0]["revision"]
        .as_str()
        .expect("snapshot revision")
}

fn refusal(reply: &Value, reason: &str) {
    assert_eq!(reply["ok"], false, "accepted invalid mutation: {reply}");
    assert!(
        reply["error"].as_str().expect("refusal").contains(reason),
        "wrong refusal: {reply}"
    );
}

#[test]
fn actual_native_transactions_preserve_complete_content_metadata_and_refresh_receipts() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        let path = fixture.root.join("work/source");
        let original = format!("HEAD\r\n{}\r\nTAIL λ\n", "x".repeat(400 * 1024));
        std::fs::write(&path, &original).expect("complete source");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751))
            .expect("executable mode");
        let metadata = std::fs::metadata(&path).expect("source metadata");
        let observed = fixture.observe("source");
        let first = request(
            "source",
            json!({"kind":"edit","old_text":"HEAD","new_text":"EDIT"}),
            revision(&observed),
            "edit-1",
        );
        let reply = fixture.mutate(&first);
        assert_eq!(reply["ok"], true, "backend={backend}: {reply}");
        let commit = &reply["result"][0];
        assert_eq!(commit["revision_contract"], 1);
        assert_eq!(
            commit["mutation"],
            json!({"id":"edit-1","outcome":"committed","durability":"synced"})
        );
        assert_eq!(commit["warning"], Value::Null);
        assert_eq!(
            commit["observation"]["source_text"],
            original.replacen("HEAD", "EDIT", 1)
        );
        assert_eq!(
            commit["observation"]["path_key"],
            observed["result"][0]["path_key"]
        );
        assert_ne!(
            commit["observation"]["revision"],
            observed["result"][0]["revision"]
        );
        let changed = std::fs::metadata(&path).expect("committed metadata");
        assert_ne!(
            changed.ino(),
            metadata.ino(),
            "replacement was not atomic inode publication"
        );
        assert_eq!(
            (changed.uid(), changed.gid(), changed.mode()),
            (metadata.uid(), metadata.gid(), metadata.mode())
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("committed source"),
            original.replacen("HEAD", "EDIT", 1)
        );
        refusal(&fixture.mutate(&first), "stale");
        let content = format!("{}\nFULL REQUEST TAIL λ\n", "y".repeat(350 * 1024));
        let second = fixture.mutate(&request(
            "source",
            json!({"kind":"replace","content":content}),
            commit["observation"]["revision"]
                .as_str()
                .expect("updated revision"),
            "replace-2",
        ));
        assert_eq!(second["ok"], true, "{second}");
        assert_eq!(second["result"][0]["observation"]["source_text"], content);
        assert_eq!(
            std::fs::read_to_string(&path).expect("whole replacement"),
            content
        );
        let small = fixture.mutate(&request(
            "source",
            json!({"kind":"replace","content":"small\n"}),
            second["result"][0]["observation"]["revision"]
                .as_str()
                .expect("replacement revision"),
            "shrink-3",
        ));
        assert_eq!(small["ok"], true, "{small}");
        assert_eq!(std::fs::read(&path).expect("shrunken file"), b"small\n");
        fixture.clean_stages();
    }
}

#[test]
fn actual_native_mutations_refuse_invalid_guards_stale_bytes_and_overlapping_matches() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        let path = fixture.root.join("work/source");
        std::fs::write(&path, "aaa ααα\r\n").expect("source");
        let valid = request(
            "source",
            json!({"kind":"replace","content":"unsafe"}),
            &format!("cfr1:{}", "0".repeat(64)),
            "operation-1",
        );
        for (field, value, reason) in [
            ("guard", Value::Null, "complete guard"),
            (
                "guard",
                json!({"revision_contract":2,"expected_revision":"absent","mutation_id":"id"}),
                "unsupported",
            ),
            (
                "guard",
                json!({"revision_contract":1,"expected_revision":"cfr2:unknown","mutation_id":"id"}),
                "algorithm",
            ),
            (
                "guard",
                json!({"revision_contract":1,"expected_revision":"absent","mutation_id":""}),
                "identity",
            ),
        ] {
            let mut changed = valid.clone();
            changed[field] = value;
            refusal(&fixture.mutate(&changed), reason);
            assert!(
                !fixture.domain().exists(),
                "invalid guard reached registry provisioning"
            );
        }
        let observed = fixture.observe("source");
        for old in ["aa", "αα", "absent"] {
            let edit = request(
                "source",
                json!({"kind":"edit","old_text":old,"new_text":"safe"}),
                revision(&observed),
                "edit-refused",
            );
            refusal(
                &fixture.mutate(&edit),
                if old == "absent" {
                    "does not match"
                } else {
                    "not unique"
                },
            );
        }
        let mtime = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        refusal(
            &fixture.mutate(&request(
                "source",
                json!({"kind":"edit","old_text":"aaa","new_text":"z".repeat(1024 * 1024 - 1)}),
                revision(&observed),
                "growth-refusal",
            )),
            "smaller than 1 MiB",
        );
        std::fs::write(&path, "bbb ααα\r\n").expect("same-size external update");
        std::fs::File::open(&path)
            .expect("source")
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .expect("restored mtime");
        refusal(
            &fixture.mutate(&request(
                "source",
                json!({"kind":"replace","content":"unsafe"}),
                revision(&observed),
                "stale-1",
            )),
            "stale",
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("intact external version"),
            "bbb ααα\r\n"
        );
        fixture.clean_stages();
    }
}

#[test]
fn actual_native_creators_use_kernel_no_replace_and_preserve_caller_umask() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        for (name, mask, mode) in [("private", "077", 0o600), ("group", "027", 0o640)] {
            let reply = fixture.mutate_mask(
                &request(
                    name,
                    json!({"kind":"create","content":"new\r\nλ"}),
                    "absent",
                    "create-1",
                ),
                Some(mask),
            );
            assert_eq!(reply["ok"], true, "backend={backend}: {reply}");
            assert_eq!(reply["result"][0]["mutation"]["outcome"], "committed");
            assert_eq!(
                std::fs::metadata(fixture.root.join("work").join(name))
                    .expect("created file")
                    .mode()
                    & 0o7777,
                mode
            );
        }
        let start = std::sync::Barrier::new(3);
        let outputs = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                start.wait();
                fixture.mutate(&request(
                    "race",
                    json!({"kind":"create","content":"left"}),
                    "absent",
                    "left",
                ))
            });
            let right = scope.spawn(|| {
                start.wait();
                fixture.mutate(&request(
                    "race",
                    json!({"kind":"create","content":"right"}),
                    "absent",
                    "right",
                ))
            });
            start.wait();
            [
                left.join().expect("left creator"),
                right.join().expect("right creator"),
            ]
        });
        assert_eq!(
            outputs.iter().filter(|value| value["ok"] == true).count(),
            1,
            "{outputs:?}"
        );
        let winner = outputs
            .iter()
            .find(|value| value["ok"] == true)
            .expect("winner");
        let loser = outputs
            .iter()
            .find(|value| value["ok"] == false)
            .expect("loser");
        refusal(loser, "exists");
        let bytes = std::fs::read_to_string(fixture.root.join("work/race")).expect("winner file");
        assert_eq!(winner["result"][0]["observation"]["source_text"], bytes);
        assert!(matches!(bytes.as_str(), "left" | "right"));
        fixture.clean_stages();
    }
}

#[test]
fn actual_native_mutations_refuse_unsupported_metadata_links_and_missing_parents() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        let path = fixture.root.join("work/source");
        std::fs::write(&path, "keep").expect("source");
        rustix::fs::setxattr(
            &path,
            "user.fixture",
            b"preserved",
            rustix::fs::XattrFlags::CREATE,
        )
        .expect("fixture xattr");
        let observed = fixture.observe("source");
        let replace = request(
            "source",
            json!({"kind":"replace","content":"unsafe"}),
            revision(&observed),
            "metadata-refusal",
        );
        refusal(&fixture.mutate(&replace), "ACLs, xattrs");
        let mut buffer = [0u8; 16];
        let size =
            rustix::fs::getxattr(&path, "user.fixture", &mut buffer[..]).expect("xattr retained");
        assert_eq!(&buffer[..size], b"preserved");
        rustix::fs::removexattr(&path, "user.fixture").expect("remove fixture xattr");
        for mode in [0o444, 0o4755] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .expect("unsupported mode");
            let observed = fixture.observe("source");
            let reply = fixture.mutate(&request(
                "source",
                json!({"kind":"replace","content":"unsafe"}),
                revision(&observed),
                "mode-refusal",
            ));
            assert_eq!(reply["ok"], false, "{reply}");
            assert!(
                reply["error"]
                    .as_str()
                    .expect("refusal")
                    .contains("owner-writable")
                    || reply["error"]
                        .as_str()
                        .expect("refusal")
                        .contains("Permission denied"),
                "{reply}"
            );
            assert_eq!(
                std::fs::metadata(&path).expect("mode unchanged").mode() & 0o7777,
                mode
            );
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("restore source mode");
        std::os::unix::fs::symlink("source", fixture.root.join("work/alias"))
            .expect("leaf symlink");
        refusal(
            &fixture.mutate(&request(
                "alias",
                json!({"kind":"replace","content":"unsafe"}),
                revision(&observed),
                "link-refusal",
            )),
            "single-linked",
        );
        refusal(
            &fixture.mutate(&request(
                "missing/child",
                json!({"kind":"create","content":"unsafe"}),
                "absent",
                "parent-refusal",
            )),
            "source binding",
        );
        assert!(!fixture.root.join("work/missing").exists());
        assert_eq!(std::fs::read(&path).expect("source unchanged"), b"keep");
        fixture.clean_stages();
    }
}

#[test]
fn actual_native_mutation_request_and_result_preserve_worst_case_escaped_content() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        let content = "\u{1}".repeat(1024 * 1024 - 1);
        let reply = fixture.mutate(&request(
            "escaped",
            json!({"kind":"create","content":content}),
            "absent",
            "escaped-create",
        ));
        assert_eq!(reply["ok"], true, "backend={backend}: {reply}");
        assert_eq!(reply["result"][0]["observation"]["source_text"], content);
        assert_eq!(
            std::fs::read(fixture.root.join("work/escaped")).expect("whole file"),
            content.as_bytes()
        );
        for content in ["x".repeat(1024 * 1024), "has\0NUL".into()] {
            refusal(
                &fixture.mutate(&request(
                    "invalid",
                    json!({"kind":"create","content":content}),
                    "absent",
                    "invalid-text",
                )),
                "smaller than 1 MiB",
            );
            assert!(!fixture.root.join("work/invalid").exists());
        }
        fixture.clean_stages();
    }
}

#[test]
fn actual_native_competing_edits_refuse_a_stale_sibling_then_preserve_both_regions() {
    for backend in [false, true] {
        let fixture = Fixture::new(backend);
        let path = fixture.root.join("work/source");
        std::fs::write(&path, "left=old\nright=old\n").expect("source");
        let observed = fixture.observe("source");
        let expected = revision(&observed);
        let start = std::sync::Barrier::new(3);
        let outputs = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                start.wait();
                fixture.mutate(&request(
                    "source",
                    json!({"kind":"edit","old_text":"left=old","new_text":"left=new"}),
                    expected,
                    "left-edit",
                ))
            });
            let right = scope.spawn(|| {
                start.wait();
                fixture.mutate(&request(
                    "source",
                    json!({"kind":"edit","old_text":"right=old","new_text":"right=new"}),
                    expected,
                    "right-edit",
                ))
            });
            start.wait();
            [
                left.join().expect("left edit"),
                right.join().expect("right edit"),
            ]
        });
        assert_eq!(
            outputs.iter().filter(|value| value["ok"] == true).count(),
            1,
            "{outputs:?}"
        );
        let loser = outputs
            .iter()
            .find(|value| value["ok"] == false)
            .expect("stale edit");
        refusal(loser, "stale");
        let current = fixture.observe("source");
        let missed = if outputs[0]["ok"] == true {
            "right"
        } else {
            "left"
        };
        let reply = fixture.mutate(&request("source", json!({"kind":"edit","old_text":format!("{missed}=old"),"new_text":format!("{missed}=new")}), revision(&current), "reread-edit"));
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(
            std::fs::read(&path).expect("both edits"),
            b"left=new\nright=new\n"
        );
        fixture.clean_stages();
    }
}
