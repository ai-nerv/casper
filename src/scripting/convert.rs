//! JSON and `MontyObject`, both ways. Lossy at the edges: NaN becomes `null`, an integer past
//! `i64` becomes a string, a tuple becomes an array, and anything JSON has no shape for becomes
//! its Python repr.

use monty_types::{MontyObject, ObjectRef};
use serde_json::Value;

pub(super) fn json_to_monty(value: Value) -> MontyObject {
    match value {
        Value::Null => MontyObject::none(),
        Value::Bool(b) => MontyObject::bool(b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => MontyObject::int(i),
            None => MontyObject::float(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => MontyObject::string(s),
        Value::Array(items) => MontyObject::list(items.into_iter().map(json_to_monty)),
        Value::Object(map) => MontyObject::dict(
            map.into_iter()
                .map(|(k, v)| (MontyObject::string(k), json_to_monty(v))),
        ),
    }
}

pub(super) fn monty_to_json(obj: ObjectRef<'_>) -> Value {
    let repr = || Value::String(obj.py_repr());
    match obj.type_name() {
        "NoneType" => Value::Null,
        "bool" => obj.as_bool().map_or_else(repr, Value::Bool),
        "int" => obj.as_int().map_or_else(repr, Value::from),
        "float" => obj
            .as_float()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        "str" => obj
            .as_str()
            .map_or_else(repr, |s| Value::String(s.to_owned())),
        "list" | "tuple" => obj.items().map_or_else(repr, |items| {
            Value::Array(items.into_iter().map(monty_to_json).collect())
        }),
        "dict" => obj.pairs().map_or_else(repr, |pairs| {
            Value::Object(
                pairs
                    .into_iter()
                    .map(|(k, v)| {
                        let key = k.as_str().map_or_else(|| k.to_string(), ToOwned::to_owned);
                        (key, monty_to_json(v))
                    })
                    .collect(),
            )
        }),
        _ => repr(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_become_their_python_kind() {
        for (value, object) in [
            (json!(null), MontyObject::none()),
            (json!(true), MontyObject::bool(true)),
            (json!(false), MontyObject::bool(false)),
            (json!(42), MontyObject::int(42)),
            (json!(-1), MontyObject::int(-1)),
            (json!(2.5), MontyObject::float(2.5)),
            (json!("hello"), MontyObject::string("hello")),
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
            assert_eq!(monty_to_json(json_to_monty(value.clone()).as_ref()), value);
        }
    }

    #[test]
    fn nan_becomes_null() {
        assert_eq!(
            monty_to_json(MontyObject::float(f64::NAN).as_ref()),
            Value::Null
        );
    }
}
