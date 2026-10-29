#![allow(clippy::expect_used, clippy::indexing_slicing)]

use super::*;
use serde_json::json;

#[test]
fn incomplete_or_failed_replies_cannot_be_classified_as_safe_mutation_refusals() {
    let complete = json!({"ok":false,"family":1,"n":0,"result":[],"error":"stale before commit","fault":"refused"});
    assert!(
        !verified_reply(&serde_json::to_vec(&complete).expect("golden refusal"))
            .expect("complete native refusal")
            .ok
    );
    for field in ["ok", "family", "n", "result", "error", "fault"] {
        let mut value = complete.clone();
        value.as_object_mut().expect("reply").remove(field);
        assert!(
            verified_reply(&serde_json::to_vec(&value).expect("incomplete")).is_err(),
            "accepted missing {field}"
        );
    }
    for (field, value) in [
        ("family", json!(2)),
        ("n", json!(1)),
        ("result", json!([null])),
        ("fault", json!("failed")),
        ("error", json!("")),
    ] {
        let mut changed = complete.clone();
        changed[field] = value;
        assert!(verified_reply(&serde_json::to_vec(&changed).expect("corrupt reply")).is_err());
    }
    let bytes = serde_json::to_vec(&complete).expect("golden bytes");
    for end in 0..bytes.len() {
        assert!(verified_reply(&bytes[..end]).is_err());
    }
    let success = json!({"ok":true,"family":1,"n":1,"result":[{}]});
    assert!(
        verified_reply(&serde_json::to_vec(&success).expect("success"))
            .expect("complete success")
            .ok
    );
}

#[test]
fn mutation_admission_rejects_missing_unknown_and_oversized_guards_before_execution() {
    let original = json!({"path":"source","operation":{"kind":"create","content":"new"},
        "guard":{"revision_contract":1,"expected_revision":"absent","mutation_id":"create-1"}});
    serde_json::from_value::<Request>(original.clone())
        .expect("golden request")
        .validate()
        .expect("creation guard");
    for (field, value) in [
        ("guard", json!(null)),
        (
            "guard",
            json!({"revision_contract":9,"expected_revision":"absent","mutation_id":"id"}),
        ),
        (
            "guard",
            json!({"revision_contract":1,"expected_revision":"absent","mutation_id":"a".repeat(129)}),
        ),
        ("path", json!("a".repeat(4097))),
        (
            "operation",
            json!({"kind":"create","content":"x".repeat(crate::source::EDIT_BYTES)}),
        ),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let request: Request = serde_json::from_value(changed).expect("typed but unsupported");
        let failure = mutation(&request).expect_err("must refuse before spawning");
        assert!(
            !failure.outcome_unknown(),
            "admission failure cannot have mutated"
        );
    }
    for (field, value) in [
        ("extra", json!(true)),
        ("operation", json!({"kind":"shell","command":"unsafe"})),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(serde_json::from_value::<Request>(changed).is_err());
    }
}
