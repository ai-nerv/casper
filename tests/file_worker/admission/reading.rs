use super::*;

fn ranged(path: &str, offset: usize, limit: usize) -> Value {
    let mut call = read(path);
    call["args"]["offset"] = json!(offset);
    call["args"]["limit"] = json!(limit);
    call
}

#[test]
fn native_read_ranges_scan_beyond_edit_and_display_bounds_without_fabricating_receipts() {
    for backend in [false, true] {
        for socket in [false, true] {
            for cbor in [false, true] {
                let fixture = Fixture::new(backend);
                install(&fixture, TOOLS);
                let text: String = (1..=40000)
                    .map(|line| format!("{line:05}: long source line beyond snapshot capacity\r\n"))
                    .collect();
                assert!(text.len() > casper::source::EDIT_BYTES);
                std::fs::write(fixture.root.join("work/large"), &text).expect("large text");
                std::fs::write(
                    fixture.root.join("work/long-line"),
                    format!("{}\nTAIL λ\n", "x".repeat(2 * casper::source::EDIT_BYTES)),
                )
                .expect("single long line");
                let mut session = Session::new(&fixture, socket);
                let reply = session.call(ranged("large", 39000, 2), cbor);
                let value = result(&reply);
                assert_ne!(value["failed"], true, "large range: {reply}");
                let said = value["said"].as_str().expect("range text");
                assert!(said.starts_with(
                    "39000: long source line beyond snapshot capacity\r\n39001: long source line beyond snapshot capacity\r"
                ), "{said}");
                assert!(said.contains("(lines 39000-39001 of 40000)"), "{said}");
                assert!(value.get("observations").is_none(), "{reply}");
                let tail = session.call(ranged("long-line", 2, 1), cbor);
                assert!(
                    result(&tail)["said"]
                        .as_str()
                        .expect("tail")
                        .starts_with("TAIL λ")
                );
                assert!(
                    result(&tail)["said"]
                        .as_str()
                        .expect("span")
                        .contains("lines 2-2 of 2")
                );
                assert!(result(&tail).get("observations").is_none());
                let capped = session.call(ranged("long-line", 1, 1), cbor);
                let said = result(&capped)["said"].as_str().expect("bounded text");
                assert!(said.len() < casper::running::MOST + 1024);
                assert!(said.contains("more bytes, not shown"), "{said}");
                assert!(
                    said.contains("(lines 1-1 of 2)"),
                    "count must come from source"
                );
                assert!(result(&capped).get("observations").is_none());
                let beyond = session.call(ranged("large", 50000, 5), cbor);
                assert!(
                    result(&beyond)["said"]
                        .as_str()
                        .expect("beyond EOF")
                        .contains("(lines 50000-40000 of 40000)")
                );
            }
        }
    }
}

#[test]
fn native_read_preserves_readonly_links_and_binary_previews_but_only_stable_editable_text_has_receipts()
 {
    for backend in [false, true] {
        for socket in [false, true] {
            for cbor in [false, true] {
                let fixture = Fixture::new(backend);
                install(&fixture, TOOLS);
                std::fs::write(fixture.root.join("work/plain"), "one\r\ntwo").expect("text");
                std::fs::write(fixture.root.join("work/linked"), "linked contents\n")
                    .expect("linked text");
                std::fs::hard_link(
                    fixture.root.join("work/linked"),
                    fixture.root.join("work/hard"),
                )
                .expect("hardlink");
                std::os::unix::fs::symlink("plain", fixture.root.join("work/soft"))
                    .expect("symlink");
                std::fs::write(fixture.root.join("work/binary"), b"raw\0\xff\nTAIL")
                    .expect("binary");
                std::os::unix::fs::symlink(
                    fixture.root.join("home/.ssh/id"),
                    fixture.root.join("work/secret"),
                )
                .expect("credential alias");
                let mut session = Session::new(&fixture, socket);
                let plain = session.call(ranged("plain", 2, 1), cbor);
                assert_eq!(result(&plain)["said"], "two\n(lines 2-2 of 2)");
                assert_eq!(result(&plain)["observations"][0]["coverage"], "snapshot");
                for (name, bytes) in [
                    ("empty", Vec::new()),
                    ("escaped", vec![1; casper::source::EDIT_BYTES - 1]),
                ] {
                    std::fs::write(fixture.root.join("work").join(name), &bytes)
                        .expect("bounded source");
                    let reply = session.call(read(name), cbor);
                    let value = result(&reply);
                    assert_ne!(value["failed"], true, "{name}: {reply}");
                    assert_eq!(
                        value["observations"][0]["coverage"], "snapshot",
                        "{name}: {reply}"
                    );
                }
                for (path, prefix) in [
                    ("soft", "one\r\ntwo"),
                    ("hard", "linked contents"),
                    ("binary", "raw\0�"),
                ] {
                    let reply = session.call(read(path), cbor);
                    let value = result(&reply);
                    assert_ne!(value["failed"], true, "{path}: {reply}");
                    assert!(
                        value["said"].as_str().expect("preview").starts_with(prefix),
                        "{path}: {reply}"
                    );
                    assert!(value.get("observations").is_none(), "{path}: {reply}");
                    assert!(value.get("absence").is_none() && value.get("mutation").is_none());
                }
                for path in ["secret", "missing"] {
                    let reply = session.call(read(path), cbor);
                    assert!(
                        reply["ok"] == false || reply["result"][0]["failed"] == true,
                        "{reply}"
                    );
                    assert!(!reply.to_string().contains("FIXTURE_SECRET"));
                    assert!(reply["result"][0].get("observations").is_none());
                }
            }
        }
    }
}
