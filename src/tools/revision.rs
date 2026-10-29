use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "supplied"
    )]
    file_revision: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "supplied"
    )]
    file_recovery: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "supplied"
    )]
    revision_contract: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "supplied"
    )]
    expected_revision: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "supplied"
    )]
    mutation_id: Option<Value>,
}

impl Revision {
    pub fn requested(&self) -> bool {
        self.file_revision.is_some() || self.file_recovery.is_some() || self.mutation_requested()
    }

    pub fn mutation_requested(&self) -> bool {
        self.revision_contract.is_some()
            || self.expected_revision.is_some()
            || self.mutation_id.is_some()
    }

    pub fn validate(&self) -> Result<Option<super::FileCapability>, String> {
        let admission = self
            .file_revision
            .as_ref()
            .map(|value| {
                let capability: super::FileCapability = serde_json::from_value(value.clone())
                    .map_err(|_| {
                        "file revision admission is incomplete or unsupported".to_owned()
                    })?;
                if capability.version != 1 {
                    return Err("file revision admission version is unsupported".to_owned());
                }
                Ok(capability)
            })
            .transpose()?;
        if self.file_recovery.is_some()
            && (self.file_recovery.as_ref().and_then(Value::as_u64) != Some(1)
                || !admission
                    .as_ref()
                    .is_some_and(|capability| capability.operation == super::FileOperation::Read))
        {
            return Err("file recovery requires supported version1 native read admission".into());
        }
        if admission
            .as_ref()
            .is_some_and(|value| value.operation == super::FileOperation::Read)
        {
            if self.mutation_requested() {
                return Err("file revision read admission cannot carry mutation guards".into());
            }
        } else if self.requested() {
            self.guard()?;
        }
        Ok(admission)
    }

    pub fn guard(&self) -> Result<Guard, String> {
        let bad = || {
            "file revision contract needs version1, a supported expected_revision and bounded mutation_id".to_owned()
        };
        if self.revision_contract.as_ref().and_then(Value::as_u64) != Some(1) {
            return Err(bad());
        }
        let expected_revision = self
            .expected_revision
            .as_ref()
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let mutation_id = self
            .mutation_id
            .as_ref()
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        if mutation_id.is_empty()
            || mutation_id.len() > 128
            || !mutation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || expected_revision != "absent"
                && !expected_revision
                    .strip_prefix("cfr1:")
                    .is_some_and(|digest| {
                        digest.len() == 64
                            && digest
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
        {
            return Err(bad());
        }
        Ok(Guard {
            expected_revision: expected_revision.into(),
            mutation_id: mutation_id.into(),
        })
    }

    pub fn recovering(&self) -> bool {
        self.file_recovery.as_ref().and_then(Value::as_u64) == Some(1)
    }
}

pub struct Guard {
    pub expected_revision: String,
    pub mutation_id: String,
}

fn supplied<'de, D: Deserializer<'de>>(input: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(input).map(Some)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use crate::tools::Call;
    use serde_json::{Value, json};

    #[test]
    fn recovery_is_read_only_versioned_and_preserves_null_presence_before_configuration() {
        let wire = json!({"tool":"look","args":{"path":"missing"},"file_revision":{"version":1,"operation":"read"},"file_recovery":1});
        let call = roundtrip(&wire);
        call.revision.validate().expect("recovery read");
        assert!(call.revision.recovering());
        for value in [Value::Null, json!(2), json!("1"), json!(true), json!({})] {
            let mut invalid = wire.clone();
            invalid["file_recovery"] = value;
            assert!(roundtrip(&invalid).revision.validate().is_err());
        }
        let mut invalid = wire.clone();
        invalid
            .as_object_mut()
            .expect("call")
            .remove("file_revision");
        assert!(roundtrip(&invalid).revision.validate().is_err());
        invalid = json!({"tool":"store","args":{},"file_revision":{"version":1,"operation":"write"},
            "file_recovery":1,"revision_contract":1,"expected_revision":"absent","mutation_id":"operation-1"});
        assert!(roundtrip(&invalid).revision.validate().is_err());
    }

    #[test]
    fn native_capability_admission_preserves_presence_and_never_ignores_null_or_unknown_versions() {
        for capability in [
            Value::Null,
            json!(true),
            json!({"version":2,"operation":"read"}),
            json!({"version":1,"operation":"unknown"}),
        ] {
            let call = roundtrip(
                &json!({"tool":"look","args":{"path":"source"},"file_revision":capability}),
            );
            assert!(call.revision.requested());
            assert!(call.revision.validate().is_err());
        }
        let read = roundtrip(
            &json!({"tool":"look","args":{"path":"source"},"file_revision":{"version":1,"operation":"read"}}),
        );
        assert_eq!(
            read.revision
                .validate()
                .expect("read admission")
                .expect("capability")
                .operation,
            super::super::FileOperation::Read
        );
        assert!(!read.revision.mutation_requested());
        let bad = roundtrip(
            &json!({"tool":"look","args":{},"file_revision":{"version":1,"operation":"read"},"expected_revision":null}),
        );
        assert!(bad.revision.validate().is_err());
        let missing = roundtrip(
            &json!({"tool":"store","args":{},"file_revision":{"version":1,"operation":"write"}}),
        );
        assert!(missing.revision.validate().is_err());
    }

    fn roundtrip(value: &Value) -> Call {
        let call: Call = serde_json::from_value(value.clone()).expect("JSON call");
        assert_eq!(serde_json::to_value(&call).expect("JSON encoding"), *value);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).expect("producer CBOR");
        let decoded: Call = ciborium::de::from_reader(bytes.as_slice()).expect("consumer CBOR");
        assert_eq!(decoded, call);
        bytes.clear();
        ciborium::ser::into_writer(&decoded, &mut bytes).expect("consumer encoding");
        let restored: Value =
            ciborium::de::from_reader(bytes.as_slice()).expect("producer decoding");
        assert_eq!(restored, *value);
        call
    }

    #[test]
    fn legacy_calls_omit_revision_fields_in_both_formats() {
        let call = roundtrip(&json!({"tool":"edit","args":{"path":"a","old":"b","new":"c"}}));
        assert!(!call.revision.requested());
    }

    #[test]
    fn revision_presence_survives_null_and_invalid_values_in_both_formats() {
        for field in ["revision_contract", "expected_revision", "mutation_id"] {
            for value in [
                Value::Null,
                json!(1),
                json!("v1"),
                json!({}),
                json!([]),
                json!(false),
            ] {
                let mut wire = json!({"tool":"edit","args":{}});
                wire[field] = value;
                assert!(roundtrip(&wire).revision.requested());
            }
        }
    }

    #[test]
    fn complete_guard_remains_outside_model_arguments() {
        let call = roundtrip(&json!({
            "tool":"edit","args":{"path":"a","old":"b","new":"c"},
            "revision_contract":1,"expected_revision":"opaque-v1","mutation_id":"operation-1"
        }));
        assert!(call.revision.requested());
        assert!(call.args.get("expected_revision").is_none());
    }
}
