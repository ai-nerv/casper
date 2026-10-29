#![allow(clippy::expect_used, clippy::indexing_slicing)]
use super::*;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

const GOLDEN: &str = include_str!("golden.json");

#[test]
fn local_identities_are_distinct_and_exhaust_without_wrapping() {
    let (producer, _) = pair();
    let mut ids = HashSet::new();
    for _ in 0..1024 {
        let id = producer.identity().expect("local identity");
        assert!(ids.insert(String::from(id)));
    }
    producer.identities.store(u64::MAX - 1, Ordering::Relaxed);
    assert!(producer.identity().is_some());
    assert!(producer.identity().is_none());
    assert!(producer.identity().is_none());
    assert_eq!(producer.identities.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(producer.loss.snapshot().exhausted, 2);
}

#[test]
fn mutation_commit_durability_observation_and_delivery_are_independent() {
    let (producer, receiver) = pair();
    let state = MutationState::Committed {
        durability: Durability::Unconfirmed,
        observation: Observation::Confirmed,
        delivery: Delivery::Unconfirmed,
    };
    let fact = Fact::Mutation { state };
    assert_eq!(
        producer.emit(|| (Correlation::default(), fact)),
        Emitted::Queued
    );
    let bytes = receiver.try_recv().expect("mutation event");
    assert_eq!(decode(&bytes).expect("schema").fact, fact);
    let value: Value = serde_json::from_slice(&bytes).expect("independent JSON");
    assert_eq!(value["fact"]["state"]["kind"], "committed");
    assert_eq!(value["fact"]["state"]["durability"], "unconfirmed");
    assert_eq!(value["fact"]["state"]["delivery"], "unconfirmed");
    for (field, replacement) in [
        ("kind", json!("refused")),
        ("kind", json!("unknown")),
        ("durability", json!("secret-warning-canary")),
        ("observation", json!("/private/path")),
        ("delivery", json!(true)),
        ("warning", json!("secret-content-canary")),
    ] {
        let mut changed = value.clone();
        changed["fact"]["state"][field] = replacement;
        assert_eq!(
            decode(&serde_json::to_vec(&changed).expect("JSON")),
            Err(DecodeError::Invalid)
        );
    }
    for state in [MutationState::Refused {}, MutationState::Unknown {}] {
        let fact = Fact::Mutation { state };
        assert_eq!(
            producer.emit(|| (Correlation::default(), fact)),
            Emitted::Queued
        );
        assert_eq!(
            decode(&receiver.try_recv().expect("event"))
                .expect("schema")
                .fact,
            fact
        );
    }
}

#[test]
fn uncertain_tool_execution_is_not_encoded_as_success() {
    let (producer, receiver) = pair();
    let mut outcomes = Vec::new();
    for outcome in [
        ToolOutcome::Succeeded,
        ToolOutcome::Failed,
        ToolOutcome::Unknown,
        ToolOutcome::Waiting,
    ] {
        assert_eq!(
            producer.emit(|| (
                Correlation::default(),
                Fact::Tool {
                    outcome,
                    duration_ms: None
                }
            )),
            Emitted::Queued
        );
        let value: Value =
            serde_json::from_slice(&receiver.try_recv().expect("event")).expect("JSON");
        outcomes.push(value["fact"]["outcome"].clone());
    }
    assert_eq!(
        outcomes,
        vec![
            json!("succeeded"),
            json!("failed"),
            json!("unknown"),
            json!("waiting")
        ]
    );
}

#[test]
fn receiver_shutdown_counts_queued_but_not_dequeued_events() {
    let (producer, receiver) = pair();
    for _ in 0..3 {
        assert_eq!(producer.emit(prepared), Emitted::Queued);
    }
    receiver.try_recv().expect("one event handed to consumer");
    assert_eq!(producer.loss.snapshot().abandoned, 0);
    drop(receiver);
    assert_eq!(producer.loss.snapshot().abandoned, 2);
    assert_eq!(producer.emit(prepared), Emitted::Closed);
    assert_eq!(producer.loss.snapshot().closed, 1);
    assert_eq!(producer.loss.snapshot().abandoned, 2);
}

#[test]
fn receiver_shutdown_racing_production_accounts_for_every_attempt() {
    let (producer, receiver) = pair();
    let producer = Arc::new(producer);
    let barrier = Arc::new(Barrier::new(2));
    let task = std::thread::spawn({
        let (producer, barrier) = (producer.clone(), barrier.clone());
        move || {
            barrier.wait();
            for _ in 0..1000 {
                assert!(matches!(
                    producer.emit(prepared),
                    Emitted::Queued | Emitted::Full | Emitted::Closed
                ));
            }
        }
    });
    barrier.wait();
    drop(receiver);
    task.join().expect("producer settled after shutdown");
    let loss = producer.loss.snapshot();
    assert_eq!(loss.abandoned + loss.full + loss.closed, 1000);
    assert_eq!(loss.encoding + loss.exhausted, 0);
}

fn identity() -> Id {
    "00112233445566778899aabbccddeeff"
        .to_owned()
        .try_into()
        .expect("opaque ID")
}
fn prepared() -> (Correlation, Fact) {
    (
        Correlation::default(),
        Fact::Tool {
            outcome: ToolOutcome::Cancelled,
            duration_ms: None,
        },
    )
}
fn pair() -> (Emitter, Receiver) {
    Emitter::new(identity(), [0, 8, 3])
}

#[test]
fn golden_is_exact_and_missing_measurements_remain_null() {
    let (producer, receiver) = pair();
    assert_eq!(producer.emit(prepared), Emitted::Queued);
    let bytes = receiver.try_recv().expect("queued event");
    assert_eq!(bytes, GOLDEN.trim_end().as_bytes());
    let event = decode(&bytes).expect("independent golden schema");
    assert_eq!(event.producer, identity());
    assert_eq!(event.sequence.get(), 1);
    assert_eq!(event.correlation, Correlation::default());
    assert_eq!(event.fact, prepared().1);
}

#[test]
fn revision_owner_kinds_fields_and_numeric_states_are_strict() {
    let original: Value = serde_json::from_str(GOLDEN).expect("golden");
    for (pointer, replacement) in [
        ("/schema", json!(2)),
        ("/schema", json!(-1)),
        ("/schema", json!("1")),
        ("/program", json!("melchior")),
        ("/sequence", json!(0)),
        ("/sequence", json!(-1)),
        ("/sequence", json!(1.5)),
        ("/program_version", json!([0, 7, 65536])),
        ("/fact/kind", json!("provider_attempt_ended")),
        ("/fact/outcome", json!("secret-error-canary")),
        ("/fact/duration_ms", json!(-1)),
        ("/fact/duration_ms", json!(1.5)),
        ("/correlation/run_id", json!("/private/secret-path")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).expect("known pointer") = replacement;
        let error = decode(&serde_json::to_vec(&changed).expect("JSON")).expect_err(pointer);
        assert_eq!(error, DecodeError::Invalid);
        assert!(!format!("{error:?}").contains("secret"));
    }
    for pointer in ["", "/fact", "/correlation"] {
        let mut changed = original.clone();
        changed
            .pointer_mut(pointer)
            .expect("object")
            .as_object_mut()
            .expect("map")
            .insert("private-content".into(), json!("secret-header-canary"));
        assert_eq!(
            decode(&serde_json::to_vec(&changed).expect("JSON")),
            Err(DecodeError::Invalid)
        );
    }
    let duplicate = GOLDEN.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
    assert_eq!(decode(duplicate.as_bytes()), Err(DecodeError::Invalid));
}

#[test]
fn opaque_identifiers_reject_hostile_and_noncanonical_labels() {
    for value in [
        "",
        "abcd",
        "00112233445566778899AABBCCDDEEFF",
        "../private/path",
        "Bearer secret-token",
        "éééééééééééééééé",
        "00112233445566778899aabbccddeeff\n",
    ] {
        assert!(Id::try_from(value.to_owned()).is_err());
    }
    let first = Id::generate().expect("OS nonce");
    let second = Id::generate().expect("new producer nonce");
    assert_ne!(first, second);
    assert_eq!(
        Id::try_from(String::from(first)).expect("canonical roundtrip"),
        first
    );
}

#[test]
fn encoded_and_decoded_limits_are_enforced_before_unbounded_growth() {
    let mut encoder = Encoded {
        bytes: [0; MAX_EVENT_BYTES],
        len: 0,
    };
    encoder
        .write_all(&[b'x'; MAX_EVENT_BYTES])
        .expect("exact limit");
    assert!(encoder.write_all(b"x").is_err());
    assert_eq!(encoder.len, MAX_EVENT_BYTES);
    let mut bytes = GOLDEN.trim_end().as_bytes().to_vec();
    bytes.resize(MAX_EVENT_BYTES, b' ');
    assert!(decode(&bytes).is_ok());
    bytes.push(b' ');
    assert_eq!(decode(&bytes), Err(DecodeError::Oversized));
}

#[test]
fn full_queue_drops_without_waiting_for_a_stalled_consumer() {
    let (producer, receiver) = pair();
    let producer = Arc::new(producer);
    for _ in 0..QUEUED_EVENTS {
        assert_eq!(producer.emit(prepared), Emitted::Queued);
    }
    let barrier = Arc::new(Barrier::new(2));
    let (done, completion) = mpsc::sync_channel(1);
    let task = std::thread::spawn({
        let (producer, barrier) = (producer.clone(), barrier.clone());
        move || {
            barrier.wait();
            assert_eq!(producer.emit(prepared), Emitted::Full);
            done.send(()).expect("useful work progressed");
        }
    });
    barrier.wait();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("producer cannot wait for consumer");
    task.join().expect("producer");
    assert_eq!(
        producer.loss.snapshot(),
        Loss {
            full: 1,
            ..Loss::default()
        }
    );
    let mut bytes = 0;
    for index in 1..=QUEUED_EVENTS {
        let event = receiver.try_recv().expect("bounded queue");
        bytes += event.len();
        assert_eq!(decode(&event).expect("event").sequence.get(), index as u64);
    }
    assert!(bytes <= QUEUED_EVENTS * MAX_EVENT_BYTES);
    assert!(receiver.try_recv().is_err());
    assert_eq!(producer.emit(prepared), Emitted::Queued);
    assert_eq!(
        decode(&receiver.try_recv().expect("after loss"))
            .expect("event")
            .sequence
            .get(),
        258
    );
}

#[test]
fn closure_exhaustion_and_counter_saturation_are_explicit() {
    let (producer, receiver) = pair();
    drop(receiver);
    assert_eq!(producer.emit(prepared), Emitted::Closed);
    assert_eq!(producer.loss.snapshot().closed, 1);
    producer.sequence.store(u64::MAX, Ordering::Relaxed);
    assert_eq!(
        producer.emit(|| panic!("exhausted producer must not prepare payload")),
        Emitted::Exhausted
    );
    assert_eq!(producer.sequence.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(producer.loss.snapshot().exhausted, 1);
    producer.loss.exhausted.store(u64::MAX, Ordering::Relaxed);
    assert_eq!(producer.emit(prepared), Emitted::Exhausted);
    assert_eq!(producer.loss.snapshot().exhausted, u64::MAX);
}

#[test]
fn concurrent_producers_have_unique_sequences_and_account_for_loss() {
    let (producer, receiver) = pair();
    let producer = Arc::new(producer);
    let workers: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn({
                let producer = producer.clone();
                move || {
                    for _ in 0..1000 {
                        assert!(matches!(
                            producer.emit(prepared),
                            Emitted::Queued | Emitted::Full
                        ));
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("producer thread");
    }
    let mut sequences = HashSet::new();
    while let Ok(bytes) = receiver.try_recv() {
        assert!(sequences.insert(decode(&bytes).expect("event").sequence));
    }
    assert_eq!(sequences.len(), QUEUED_EVENTS);
    assert_eq!(producer.loss.snapshot().full, 4000 - QUEUED_EVENTS as u64);
    assert_eq!(producer.sequence.load(Ordering::Relaxed), 4001);
}

#[test]
fn process_emission_starts_disabled_and_has_only_one_local_receiver() {
    assert_eq!(
        emit(|| panic!("disabled must not prepare payload")),
        Emitted::Disabled
    );
    assert_eq!(loss(), Loss::default());
    let receiver = enable_local().expect("explicit local activation");
    assert!(matches!(enable_local(), Err(EnableError::AlreadyEnabled)));
    assert_eq!(emit(prepared), Emitted::Queued);
    assert!(decode(&receiver.try_recv().expect("local event")).is_ok());
    drop(receiver);
    assert_eq!(emit(prepared), Emitted::Closed);
    assert_eq!(loss().closed, 1);
}

#[test]
fn typed_owner_facts_roundtrip_without_arbitrary_measurement_maps() {
    let (producer, receiver) = pair();
    for fact in [
        Fact::Tool {
            outcome: ToolOutcome::Started,
            duration_ms: elapsed(Instant::now()),
        },
        Fact::Mutation {
            state: MutationState::Committed {
                durability: Durability::Unconfirmed,
                observation: Observation::Confirmed,
                delivery: Delivery::Unconfirmed,
            },
        },
        Fact::ToolServer {
            outcome: ServerOutcome::AuthenticationRequired,
        },
    ] {
        assert_eq!(
            producer.emit(|| (
                Correlation {
                    turn_id: Some(identity()),
                    ..Correlation::default()
                },
                fact
            )),
            Emitted::Queued
        );
        let event = decode(&receiver.try_recv().expect("event")).expect("schema");
        assert_eq!(event.fact, fact);
        assert_eq!(event.correlation.turn_id, Some(identity()));
    }
}

#[test]
fn server_outcomes_have_closed_content_free_encodings() {
    use ServerOutcome::*;
    for (outcome, name) in [
        (Connecting, "connecting"),
        (Connected, "connected"),
        (CloseRequested, "close_requested"),
        (Disconnected, "disconnected"),
        (AuthenticationRequired, "authentication_required"),
        (Refused, "refused"),
        (Failed, "failed"),
        (Unknown, "unknown"),
    ] {
        let fact = Fact::ToolServer { outcome };
        let encoded = format!(r#"{{"kind":"tool_server","outcome":"{name}"}}"#);
        assert_eq!(serde_json::to_string(&fact).expect("encoding"), encoded);
        assert_eq!(
            serde_json::from_str::<Fact>(&encoded).expect("decoding"),
            fact
        );
        for field in ["url", "command", "error", "pid", "headers", "name"] {
            let hostile =
                format!(r#"{{"kind":"tool_server","outcome":"{name}","{field}":"private"}}"#);
            assert!(serde_json::from_str::<Fact>(&hostile).is_err());
        }
    }
    assert!(serde_json::from_str::<Fact>(r#"{"kind":"tool_server","outcome":"reaped"}"#).is_err());
}
