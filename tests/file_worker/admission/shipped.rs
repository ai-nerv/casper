use super::*;

const SHIPPED: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/tools.lua"));

fn look(path: &str) -> Value {
    json!({"tool":"read","args":{"path":path},"file_revision":{"version":1,"operation":"read"}})
}

fn guarded(tool: &str, args: Value, expected: &str, id: &str) -> Value {
    json!({"tool":tool,"args":args,
        "file_revision":{"version":1,"operation":tool},
        "revision_contract":1,"expected_revision":expected,"mutation_id":id})
}

fn receipt(reply: &Value) -> &str {
    result(reply)["observations"][0]["revision"]
        .as_str()
        .expect("delivered native observation")
}

#[test]
fn shipped_file_tools_require_native_guards_preserve_presentation_and_never_use_legacy_io() {
    for backend in [false, true] {
        for socket in [false, true] {
            for cbor in [false, true] {
                let fixture = Fixture::new(backend);
                install(&fixture, SHIPPED);
                let mut session = Session::new(&fixture, socket);
                let unguarded = session.call(
                    json!({"tool":"write","args":{"path":"unguarded","contents":"unsafe"}}),
                    cbor,
                );
                assert_eq!(
                    unguarded["ok"], false,
                    "unguarded shipped mutation ran: {unguarded}"
                );
                assert!(!fixture.root.join("work/unguarded").exists());
                let cards = session.request("tools", Vec::new(), cbor);
                for name in ["read", "edit", "write"] {
                    let card = cards["result"]
                        .as_array()
                        .expect("cards")
                        .iter()
                        .find(|card| card["name"] == name)
                        .expect("shipped card");
                    assert_eq!(card["file_revision"], json!({"version":1,"operation":name}));
                    assert_eq!(card["group"], "files");
                    assert!(card.get("script").is_none(), "scriptability is internal");
                    assert_ne!(card["deferred"], true);
                    assert_eq!(card["needs"], if name == "read" { "read" } else { "write" });
                }
                install(
                    &fixture,
                    &format!(
                        "{SHIPPED}\ncasper.source=nil; casper.replace_source=nil; casper.exec=nil"
                    ),
                );
                std::fs::write(fixture.root.join("work/source"), "a\nb\nc\n").expect("source");
                let read = session.call(look("source"), cbor);
                let value = result(&read);
                assert_ne!(value["failed"], true, "{read}");
                assert_eq!(value["said"], "a\nb\nc");
                assert_eq!(value["brief"], "read source (3 lines)");
                assert_eq!(value["back"], "read source");
                assert!(value.get("shown").is_some());
                let nested = session.call(
                    json!({"tool":"execute","args":{"code":"print(await read(path='source'))"}}),
                    cbor,
                );
                assert_ne!(result(&nested)["failed"], true, "{nested}");
                assert!(
                    result(&nested)["said"]
                        .as_str()
                        .expect("nested read")
                        .contains("a\nb\nc")
                );
                assert!(result(&nested).get("observations").is_none());
                let denied = session.call(json!({"tool":"execute","args":{"code":"await write(path='nested-write', contents='unsafe')"}}), cbor);
                assert_eq!(result(&denied)["failed"], true, "{denied}");
                assert!(!fixture.root.join("work/nested-write").exists());
                let changed = session.call(
                    guarded(
                        "edit",
                        json!({"path":"source","old":"b","new":"B\nB2"}),
                        receipt(&read),
                        "shipped-edit",
                    ),
                    cbor,
                );
                let value = result(&changed);
                assert_ne!(value["failed"], true, "{changed}");
                assert_eq!(
                    value["said"],
                    "--- source\n+++ source\n@@ -1,3 +1,4 @@\n a\n-b\n+B\n+B2\n c"
                );
                assert_eq!(value["brief"], "edited source (+2 -1 at line 2)");
                assert_eq!(value["back"], "read source");
                assert_eq!(value["mutation"]["outcome"], "committed");
                assert_eq!(
                    std::fs::read(fixture.root.join("work/source")).expect("edited"),
                    b"a\nB\nB2\nc\n"
                );
                let stale = session.call(
                    guarded(
                        "edit",
                        json!({"path":"source","old":"c","new":"unsafe"}),
                        receipt(&read),
                        "shipped-stale",
                    ),
                    cbor,
                );
                assert_eq!(result(&stale)["mutation"]["outcome"], "refused");
                let replaced = session.call(
                    guarded(
                        "write",
                        json!({"path":"source","contents":"shrunk\n"}),
                        receipt(&changed),
                        "shipped-replace",
                    ),
                    cbor,
                );
                assert_eq!(
                    result(&replaced)["brief"],
                    "wrote source (1 lines, replaced)"
                );
                assert_eq!(
                    std::fs::read(fixture.root.join("work/source")).expect("replaced"),
                    b"shrunk\n"
                );
                let created = session.call(
                    guarded(
                        "write",
                        json!({"path":"created","contents":"x = 1\ny = 2\n"}),
                        "absent",
                        "shipped-create",
                    ),
                    cbor,
                );
                assert_eq!(
                    result(&created)["said"],
                    "wrote created: 2 lines, 12 bytes, new file"
                );
                assert_eq!(
                    result(&created)["brief"],
                    "wrote created (2 lines, new file)"
                );
                assert!(result(&created).get("shown").is_some());
                let occupied = session.call(
                    guarded(
                        "write",
                        json!({"path":"created","contents":"unsafe"}),
                        "absent",
                        "shipped-no-replace",
                    ),
                    cbor,
                );
                assert_eq!(result(&occupied)["mutation"]["outcome"], "refused");
                assert_eq!(
                    std::fs::read(fixture.root.join("work/created")).expect("created"),
                    b"x = 1\ny = 2\n"
                );
                let missing = session.call(look("missing"), cbor);
                assert_eq!(result(&missing)["failed"], true);
                assert_eq!(result(&missing)["keep"], true);
                assert!(
                    !result(&missing)["brief"]
                        .as_str()
                        .expect("failure stub")
                        .is_empty()
                );
                let parent = session.call(
                    guarded(
                        "write",
                        json!({"path":"no-parent/leaf","contents":"unsafe"}),
                        "absent",
                        "shipped-parent",
                    ),
                    cbor,
                );
                assert_eq!(result(&parent)["mutation"]["outcome"], "refused");
                assert!(!fixture.root.join("work/no-parent").exists());
                std::fs::write(fixture.root.join("work/overlap"), "aaaa")
                    .expect("ambiguous source");
                let overlap = session.call(look("overlap"), cbor);
                let refused = session.call(
                    guarded(
                        "edit",
                        json!({"path":"overlap","old":"aaa","new":"unsafe"}),
                        receipt(&overlap),
                        "shipped-overlap",
                    ),
                    cbor,
                );
                assert_eq!(result(&refused)["mutation"]["outcome"], "refused");
                assert_eq!(
                    std::fs::read(fixture.root.join("work/overlap")).expect("unchanged"),
                    b"aaaa"
                );
                std::fs::write(
                    fixture.root.join("work/large"),
                    format!("{}\nLATE λ\n", "x".repeat(2 * casper::source::EDIT_BYTES)),
                )
                .expect("large");
                let mut range = look("large");
                range["args"]["offset"] = json!(2);
                range["args"]["limit"] = json!(1);
                let preview = session.call(range, cbor);
                assert!(
                    result(&preview)["said"]
                        .as_str()
                        .expect("range")
                        .starts_with("LATE λ\n(lines 2-2 of 2)")
                );
                assert!(result(&preview).get("observations").is_none());
            }
        }
    }
}
