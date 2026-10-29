use super::*;
use crate::scratch::Scratch;

struct Chunks<'a> {
    bytes: &'a [u8],
    width: usize,
}

impl Read for Chunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> Result<usize> {
        let count = self.width.min(output.len());
        self.bytes
            .read(output.get_mut(..count).expect("bounded chunk"))
    }
}

#[test]
fn range_scanning_preserves_actual_line_semantics_independently_of_chunk_boundaries() {
    for text in [
        "",
        "\n",
        "\n\n",
        "one",
        "one\n",
        "one\r\ntwo\r\n",
        "λ\r\n\nend",
    ] {
        let lines: Vec<_> = text.split_terminator('\n').collect();
        for offset in 1..=lines.len() + 3 {
            for limit in 1..=5 {
                for width in [1, 2, 3, 7, 8192] {
                    let result = scan(
                        Chunks {
                            bytes: text.as_bytes(),
                            width,
                        },
                        offset,
                        limit,
                        true,
                    )
                    .expect("bounded scanning");
                    let start = offset - 1;
                    let last = start.saturating_add(limit).min(lines.len());
                    let expected = lines.get(start..last).unwrap_or_default().join("\n");
                    assert_eq!(
                        result.lines,
                        lines.len(),
                        "{text:?} offset {offset} width {width}"
                    );
                    assert_eq!(result.bytes, text.len());
                    assert_eq!(result.held, expected.as_bytes());
                    assert_eq!(result.selected, expected.len());
                    assert_eq!(result.source.as_deref(), Some(text.as_bytes()));
                }
            }
        }
    }
}

#[test]
fn huge_lines_and_late_ranges_have_bounded_retention_and_explicit_utf8_safe_omissions() {
    let text = format!("{}\nTAIL λ\n", "€".repeat(crate::source::EDIT_BYTES));
    let result = scan(text.as_bytes(), 2, 1, true).expect("skip huge first line");
    assert_eq!(result.held, "TAIL λ".as_bytes());
    assert_eq!(result.lines, 2);
    assert!(result.source.is_none());
    let first = scan(text.as_bytes(), 1, 1, true).expect("bounded first line");
    assert!(first.held.len() <= crate::running::MOST);
    assert!(first.source.is_none());
    assert_eq!(first.selected, "€".len() * crate::source::EDIT_BYTES);
    assert!(
        std::str::from_utf8(&first.held).is_ok(),
        "display split a codepoint"
    );
    assert!(first.selected - first.held.len() > 0);
    let binary = [0xff; crate::running::MOST + 3];
    let result = scan(binary.as_slice(), 1, 1, false).expect("binary bounded preview");
    assert_eq!(result.held.len(), crate::running::MOST);
    assert_eq!(result.selected - result.held.len(), 3);
    assert!(String::from_utf8_lossy(&result.held).len() <= crate::running::MOST * 3);
}

#[test]
fn readonly_read_receipts_match_complete_snapshot_and_refuse_unstable_binding_or_source() {
    let root = Scratch::new("native-reading", "stable");
    let path = root.join("source");
    std::fs::write(&path, "one\r\ntwo").expect("source");
    let resolved = Resolved::bind(path.to_str().expect("path")).expect("binding");
    let reading = resolved.read(2, 1).expect("ranged acquisition");
    let original = resolved.capture().expect("complete snapshot");
    let snapshot = reading.snapshot.as_ref().expect("editable receipt");
    assert_eq!(snapshot.revision, original.revision);
    assert_eq!(snapshot.source_text, "one\r\ntwo");
    assert_eq!(reading.text, "two");
    reading
        .validate(&Key::from_wire(&reading.path_key).expect("key"), 2, 1)
        .expect("valid");
    assert!(
        resolved
            .read_with(1, 1, || std::fs::write(&path, "modified source"))
            .is_err()
    );
    std::fs::write(root.join("binary"), b"one\0\xff").expect("noneditable");
    let binary = Resolved::bind(root.join("binary").to_str().expect("path")).expect("binding");
    assert!(
        binary
            .read(1, 1)
            .expect("readonly binary")
            .snapshot
            .is_none()
    );
    let link = root.join("link");
    std::os::unix::fs::symlink("source", &link).expect("readonly symlink");
    let linked = Resolved::bind(link.to_str().expect("path")).expect("binding");
    assert!(
        linked
            .read(1, 1)
            .expect("readonly symlink")
            .snapshot
            .is_none()
    );
    assert!(
        linked
            .read_with(1, 1, || {
                std::fs::remove_file(&link)?;
                std::os::unix::fs::symlink("binary", &link)
            })
            .is_err(),
        "changed leaf alias produced a receipt"
    );
    std::fs::hard_link(&path, root.join("hard")).expect("hardlink");
    assert!(
        resolved
            .read(1, 1)
            .expect("readonly hardlink")
            .snapshot
            .is_none()
    );
    let directory = root.join("parent");
    std::fs::create_dir(&directory).expect("parent");
    std::fs::write(directory.join("source"), "source").expect("parented source");
    let binding =
        Resolved::bind(directory.join("source").to_str().expect("path")).expect("binding");
    assert!(
        binding
            .read_with(1, 1, || {
                std::fs::rename(&directory, root.join("moved"))?;
                std::fs::create_dir(&directory)
            })
            .is_err(),
        "retargeted parent produced a receipt"
    );
}

#[test]
fn invalid_read_ranges_guards_and_mismatched_snapshot_displays_cannot_authorize_mutations() {
    use super::super::{Guard, Operation, Request};
    for (offset, limit) in [(0, 1), (1, 0), (1, 5001)] {
        let request = Request {
            path: "source".into(),
            operation: Operation::Read { offset, limit },
            guard: None,
        };
        assert!(request.validate().is_err());
    }
    let request = Request {
        path: "source".into(),
        operation: Operation::Read {
            offset: 1,
            limit: 1,
        },
        guard: Some(Guard {
            revision_contract: 1,
            expected_revision: "absent".into(),
            mutation_id: "id".into(),
        }),
    };
    assert!(request.validate().is_err());
    let root = Scratch::new("native-reading", "envelope");
    let path = root.join("source");
    std::fs::write(&path, "complete\nsource").expect("source");
    let resolved = Resolved::bind(path.to_str().expect("path")).expect("binding");
    let reading = resolved.read(1, 1).expect("read");
    let key = Key::from_wire(&reading.path_key).expect("key");
    let original = serde_json::to_value(reading).expect("wire");
    for (field, value) in [
        ("revision_contract", serde_json::json!(2)),
        ("path_key", serde_json::json!("wrong")),
        ("first_line", serde_json::json!(2)),
        ("last_line", serde_json::json!(2)),
        ("total_lines", serde_json::json!(3)),
        ("text", serde_json::json!("different version")),
        ("omitted_bytes", serde_json::json!(1)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let reading: Reading = serde_json::from_value(changed).expect("typed reading");
        assert!(
            reading.validate(&key, 1, 1).is_err(),
            "accepted changed {field}"
        );
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> Result<usize> {
            Err(Error::other("fixture acquisition failed"))
        }
    }
    assert!(
        scan(Broken, 1, 1, true).is_err(),
        "read error produced a partial snapshot"
    );
}
