#![allow(clippy::unwrap_used)]

use super::*;
use crate::workflows::{Candidate, ENTRIES_MOST, FILE_MOST, METADATA_MOST};
use sha2::{Digest, Sha256};

fn request(operation: Operation) -> Request {
    Request {
        version: 1,
        root: "/fixture/skills".into(),
        operation,
    }
}

#[test]
fn request_rejects_unknown_duplicate_and_trailing_data() {
    for input in [
        r#"{"version":1,"root":"/a","operation":{"kind":"catalog"},"extra":0}"#,
        r#"{"version":1,"version":1,"root":"/a","operation":{"kind":"catalog"}}"#,
        r#"{"version":1,"root":"/a","operation":{"kind":"catalog","extra":0}}"#,
        r#"{"version":1,"root":"/a","operation":{"kind":"load","name":"a","extra":0}}"#,
        r#"{"version":1,"root":"/a","operation":{"kind":"catalog"}} {}"#,
    ] {
        assert_eq!(
            Request::read(input.as_bytes()),
            Err(Failure::Protocol),
            "{input}"
        );
    }
}

#[test]
fn request_identity_and_input_bounds_are_checked() {
    let valid = request(Operation::Catalog {});
    assert_eq!(
        Request::read(serde_json::to_vec(&valid).unwrap().as_slice()),
        Ok(valid.clone())
    );
    let mut invalid = valid.clone();
    invalid.version = 2;
    assert_eq!(invalid.validate(), Err(Failure::Protocol));
    for root in ["relative", "/a/../b", "/a\0b"] {
        invalid = valid.clone();
        invalid.root = root.into();
        assert_eq!(invalid.validate(), Err(Failure::InvalidPath));
    }
    assert_eq!(
        request(Operation::Load {
            name: "../escape".into()
        })
        .validate(),
        Err(Failure::InvalidPath)
    );
    let mut input = serde_json::to_vec(&valid).unwrap();
    input.resize(INPUT_MOST, b' ');
    assert!(Request::read(input.as_slice()).is_ok());
    input.push(b' ');
    assert_eq!(Request::read(input.as_slice()), Err(Failure::Protocol));
}

#[test]
fn response_is_bound_to_version_root_operation_and_shape() {
    let req = request(Operation::Catalog {});
    let mut reply = req.response(Err(Failure::Changed));
    assert_eq!(reply.validate(&req), Ok(()));
    reply.version = 2;
    assert_eq!(reply.validate(&req), Err(Failure::Protocol));
    reply = req.response(Err(Failure::Changed));
    reply.root = "/different".into();
    assert_eq!(reply.validate(&req), Err(Failure::Protocol));
    reply = req.response(Err(Failure::Changed));
    reply.operation = Operation::Load { name: "a".into() };
    assert_eq!(reply.validate(&req), Err(Failure::Protocol));
    reply = req.response(Ok(Payload::Source(Source {
        text: String::new(),
        sha256: format!("{:x}", Sha256::digest(b"")),
    })));
    assert_eq!(reply.validate(&req), Err(Failure::Protocol));
    let mut bytes = serde_json::to_string(&req.response(Err(Failure::Changed))).unwrap();
    bytes.pop();
    bytes.push_str(",\"unknown\":true}");
    assert!(serde_json::from_str::<Response>(&bytes).is_err());
}

#[test]
fn catalog_response_rejects_duplicates_order_names_and_bounds() {
    let req = request(Operation::Catalog {});
    let validate = |entries, invalid_names| {
        req.response(Ok(Payload::Catalog(Catalog {
            entries,
            invalid_names,
        })))
        .validate(&req)
    };
    let entry = |name: &str| Candidate {
        name: name.into(),
        metadata: Err(Failure::Metadata),
    };
    assert_eq!(validate(vec![entry("a"), entry("b")], 0), Ok(()));
    for entries in [
        vec![entry("a"), entry("a")],
        vec![entry("b"), entry("a")],
        vec![entry("UPPER")],
    ] {
        assert_eq!(validate(entries, 0), Err(Failure::Protocol));
    }
    assert_eq!(validate(Vec::new(), ENTRIES_MOST), Ok(()));
    assert_eq!(
        validate(vec![entry("a")], ENTRIES_MOST),
        Err(Failure::Protocol)
    );
    assert_eq!(validate(Vec::new(), usize::MAX), Err(Failure::Protocol));
    for metadata in ["x".repeat(METADATA_MOST + 1), "a\0b".into()] {
        assert_eq!(
            validate(
                vec![Candidate {
                    name: "a".into(),
                    metadata: Ok(metadata)
                }],
                0
            ),
            Err(Failure::Protocol)
        );
    }
}

#[test]
fn selected_source_requires_exact_digest_and_size() {
    let req = request(Operation::Load { name: "a".into() });
    for (text, expected) in [
        ("x".repeat(FILE_MOST), Ok(())),
        ("x".repeat(FILE_MOST + 1), Err(Failure::Protocol)),
        ("a\0b".into(), Err(Failure::Protocol)),
    ] {
        let source = Source {
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            text,
        };
        assert_eq!(
            req.response(Ok(Payload::Source(source))).validate(&req),
            expected
        );
    }
    let source = Source {
        text: "body".into(),
        sha256: "0".repeat(64),
    };
    assert_eq!(
        req.response(Ok(Payload::Source(source))).validate(&req),
        Err(Failure::Protocol)
    );
}

#[test]
fn maximally_escaped_catalog_fits_encoded_reply_limit() {
    let req = request(Operation::Catalog {});
    let entries = (0..ENTRIES_MOST)
        .map(|n| Candidate {
            name: format!("a{n:03}"),
            metadata: Ok("\u{1}".repeat(METADATA_MOST)),
        })
        .collect();
    let reply = req.response(Ok(Payload::Catalog(Catalog {
        entries,
        invalid_names: 0,
    })));
    assert_eq!(reply.validate(&req), Ok(()));
    assert!(serde_json::to_vec(&reply).unwrap().len() < REPLY_MOST);
}

#[test]
fn captured_binding_rejects_replaced_root_and_other_request() {
    let fixture = crate::scratch::Scratch::new("workflow", "binding");
    let path = fixture.join("skills");
    std::fs::create_dir(&path).unwrap();
    let root = Root::open(&path, &Budget::new()).unwrap();
    let binding = Binding::of(&path, &root.fd).unwrap();
    let mut req = request(Operation::Catalog {});
    req.root = path.clone();
    assert!(acquire(&req, &binding).is_ok());
    req.root = fixture.to_path_buf();
    assert_eq!(acquire(&req, &binding), Err(Failure::Changed));
    req.root = path.clone();
    std::fs::rename(&path, fixture.join("old")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(acquire(&req, &binding), Err(Failure::Changed));
}
