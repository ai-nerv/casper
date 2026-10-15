//! JSON and `MontyObject`, both ways. Lossy at the edges: NaN becomes `null`, an integer past
//! `i64` becomes a string, and a tuple becomes an array.

use monty_types::MontyObject;
use serde_json::Value;

pub(super) fn json_to_monty(value: Value) -> MontyObject {
    match value {
        Value::Null => MontyObject::None,
        Value::Bool(b) => MontyObject::Bool(b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => MontyObject::Int(i),
            None => MontyObject::Float(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => MontyObject::String(s),
        Value::Array(items) => MontyObject::List(items.into_iter().map(json_to_monty).collect()),
        Value::Object(map) => MontyObject::Dict(
            map.into_iter()
                .map(|(k, v)| (MontyObject::String(k), json_to_monty(v)))
                .collect::<Vec<_>>()
                .into(),
        ),
    }
}

pub(super) fn monty_to_json(obj: &MontyObject) -> Value {
    match obj {
        MontyObject::None => Value::Null,
        MontyObject::Bool(b) => Value::Bool(*b),
        MontyObject::Int(i) => Value::Number((*i).into()),
        MontyObject::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
        }
        MontyObject::String(s) | MontyObject::Repr(s) => Value::String(s.clone()),
        MontyObject::List(items) | MontyObject::Tuple(items) => {
            Value::Array(items.iter().map(monty_to_json).collect())
        }
        MontyObject::Dict(pairs) => Value::Object(
            pairs
                .into_iter()
                .map(|(k, v)| {
                    let key = match k {
                        MontyObject::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (key, monty_to_json(v))
                })
                .collect(),
        ),
        MontyObject::Bytes(b) => Value::Array(b.iter().map(|&byte| Value::from(byte)).collect()),
        MontyObject::BigInt(big) => {
            i64::try_from(big).map_or_else(|_| Value::String(big.to_string()), Value::from)
        }
        _ => Value::String(obj.py_repr()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_become_their_python_kind() {
        for (value, object) in [
            (json!(null), MontyObject::None),
            (json!(true), MontyObject::Bool(true)),
            (json!(false), MontyObject::Bool(false)),
            (json!(42), MontyObject::Int(42)),
            (json!(-1), MontyObject::Int(-1)),
            (json!(2.5), MontyObject::Float(2.5)),
            (json!("hello"), MontyObject::String("hello".into())),
        ] {
            assert_eq!(json_to_monty(value), object);
        }
    }

    #[test]
    fn a_round_trip_keeps_the_value() {
        for value in [
            json!(null),
            json!(true),
            json!(42),
            json!(2.5),
            json!("text"),
            json!([1, 2, 3]),
            json!({"a": 1, "b": [true, null]}),
            json!([]),
            json!({}),
            json!({"key": [1, "two", null]}),
            json!(i64::MAX),
        ] {
            assert_eq!(monty_to_json(&json_to_monty(value.clone())), value);
        }
    }

    #[test]
    fn nan_becomes_null() {
        assert_eq!(monty_to_json(&MontyObject::Float(f64::NAN)), Value::Null);
    }
}
