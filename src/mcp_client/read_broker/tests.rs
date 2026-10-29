use super::*;
use serde_json::json;

fn envelope(sequence: u64) -> Envelope {
    Envelope::new(
        sequence,
        Path::new("/work"),
        "read",
        json!({"path":"one"}),
        ReadData::Text(crate::tools::ReadText {
            source_text: None,
            text: "one".into(),
            source_bytes: 3,
            total_lines: 1,
            first_line: 1,
            last_line: 1,
            omitted_bytes: 0,
        }),
    )
}

#[test]
fn negotiation_requires_the_exact_supported_read_context() {
    let root = Path::new("/work");
    assert!(!negotiated(None, root).expect("no extension"));
    assert!(negotiated(Some(&descriptor(root)), root).expect("supported"));
    for value in [
        json!({}),
        json!({"version":2,"operation":"read","root":"/work"}),
        json!({"version":1,"operation":"edit","root":"/work"}),
        json!({"version":1,"operation":"read","root":"/other"}),
        json!({"version":1,"operation":"read","root":"/work","grants":{}}),
    ] {
        assert!(negotiated(Some(&value), root).is_err(), "{value}");
    }
}

#[test]
fn envelopes_are_bound_one_use_and_keep_a_bounded_out_of_order_window() {
    let mut consumed = Consumed::default();
    let args = json!({"path":"one"});
    for (root, tool, args) in [
        ("/other", "read", args.clone()),
        ("/work", "edit", args.clone()),
        ("/work", "read", json!({"path":"two"})),
    ] {
        assert!(
            consumed
                .take(envelope(1), Path::new(root), tool, &args)
                .is_err()
        );
    }
    let take = |state: &mut Consumed, sequence| {
        state.take(envelope(sequence), Path::new("/work"), "read", &args)
    };
    assert!(take(&mut consumed, 0).is_err());
    assert!(take(&mut consumed, 2).is_ok());
    assert!(take(&mut consumed, 1).is_ok());
    assert!(take(&mut consumed, 1).is_err());
    for sequence in 3..3000 {
        assert!(take(&mut consumed, sequence).is_ok());
    }
    assert!(take(&mut consumed, 4).is_err());
    assert_eq!(consumed.seen.len(), 1024);
}

#[test]
fn broker_arguments_cannot_carry_mutation_recovery_or_unbounded_reads() {
    for args in [
        json!({"path":"one","file_recovery":1}),
        json!({"path":"one","old":"x","new":"y"}),
        json!({"path":"one","offset":0}),
        json!({"path":"one","limit":5001}),
        json!({"path":""}),
    ] {
        assert!(ReadArgs::parse(&args).is_err(), "{args}");
    }
    let input = vec![b' '; 32 * 1024 + 1];
    assert!(
        acquire_input(input.as_slice())
            .expect_err("bound")
            .contains("bound")
    );
}
