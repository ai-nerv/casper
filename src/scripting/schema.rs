//! A tool's JSON Schema, read two ways: as the Python signature a script is shown, and as the
//! check a script's arguments pass before the tool runs. Only what tool schemas here use is read:
//! `type`, `enum`, `minimum`, `maximum` and `required`.

use serde_json::{Map, Value};

/// `- read(path: str, limit: int = None, offset: int = None) -> str`: required first, then by name.
#[must_use]
pub fn signature(name: &str, schema: &Value) -> String {
    let properties = schema.get("properties").and_then(Value::as_object);
    let required = required(schema);
    let mut names: Vec<&String> = properties.map(|p| p.keys().collect()).unwrap_or_default();
    names.sort_by_key(|n| (!required.contains(&n.as_str()), n.as_str()));
    let params: Vec<String> = names
        .into_iter()
        .map(|n| {
            let kind = properties
                .and_then(|p| p.get(n))
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str)
                .map_or("any", python);
            if required.contains(&n.as_str()) {
                format!("{n}: {kind}")
            } else {
                format!("{n}: {kind} = None")
            }
        })
        .collect();
    format!("- {name}({}) -> str", params.join(", "))
}

fn python(kind: &str) -> &'static str {
    match kind {
        "string" => "str",
        "integer" => "int",
        "number" => "float",
        "boolean" => "bool",
        "array" => "list",
        "object" => "dict",
        _ => "any",
    }
}

fn required(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The arguments a script passed, held to `schema` as Python would hold a call to a signature.
/// An argument given as `None` is left out, which is what `= None` in the signature promises.
///
/// # Errors
/// An argument the tool does not take, one it needs and was not given, or one of the wrong kind.
pub fn checked(name: &str, schema: &Value, args: Value) -> Result<Value, String> {
    let Value::Object(given) = args else {
        return Err(format!("{name}() takes keyword arguments"));
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    let mut kept = Map::new();
    for (key, value) in given {
        let Some(property) = properties.and_then(|p| p.get(&key)) else {
            return Err(format!(
                "{name}() got an unexpected keyword argument '{key}'"
            ));
        };
        if value.is_null() {
            continue;
        }
        fits(property, &value).map_err(|why| format!("{name}(): '{key}' {why}"))?;
        kept.insert(key, value);
    }
    if let Some(missing) = required(schema)
        .into_iter()
        .find(|r| !kept.contains_key(*r))
    {
        return Err(format!("{name}() missing required argument '{missing}'"));
    }
    Ok(Value::Object(kept))
}

fn fits(property: &Value, value: &Value) -> Result<(), String> {
    let kinds: Vec<&str> = match property.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !kinds.is_empty() && !kinds.iter().any(|kind| is(kind, value)) {
        let wanted: Vec<&str> = kinds.iter().map(|kind| python(kind)).collect();
        return Err(format!(
            "must be {}, not {}",
            wanted.join(" or "),
            kind_of(value)
        ));
    }
    if let Some(choices) = property.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        return Err(format!("must be one of {}", Value::Array(choices.clone())));
    }
    let number = value.as_f64();
    if let (Some(n), Some(least)) = (number, property.get("minimum").and_then(Value::as_f64))
        && n < least
    {
        return Err(format!("must be at least {least}"));
    }
    if let (Some(n), Some(most)) = (number, property.get("maximum").and_then(Value::as_f64))
        && n > most
    {
        return Err(format!("must be at most {most}"));
    }
    Ok(())
}

fn is(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "None",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer", "minimum": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 5000 },
                "mode": { "type": "string", "enum": ["a", "b"] },
            },
            "required": ["path"],
        })
    }

    #[test]
    fn a_signature_puts_what_is_required_first_and_the_rest_by_name() {
        assert_eq!(
            signature("read", &read()),
            "- read(path: str, limit: int = None, mode: str = None, offset: int = None) -> str"
        );
        assert_eq!(
            signature("pwd", &json!({"type": "object"})),
            "- pwd() -> str"
        );
    }

    #[test]
    fn arguments_that_fit_pass_and_none_means_left_out() {
        let checked = checked(
            "read",
            &read(),
            json!({"path": "a", "limit": null, "offset": 2}),
        );
        assert_eq!(checked, Ok(json!({"path": "a", "offset": 2})));
    }

    #[test]
    fn arguments_that_do_not_fit_are_refused_the_way_python_would() {
        for (args, said) in [
            (
                json!({"path": "a", "colour": 1}),
                "unexpected keyword argument 'colour'",
            ),
            (json!({"limit": 3}), "missing required argument 'path'"),
            (json!({"path": null}), "missing required argument 'path'"),
            (json!({"path": 7}), "'path' must be str, not int"),
            (
                json!({"path": "a", "limit": 1.5}),
                "'limit' must be int, not float",
            ),
            (
                json!({"path": "a", "limit": 0}),
                "'limit' must be at least 1",
            ),
            (
                json!({"path": "a", "limit": 9000}),
                "'limit' must be at most 5000",
            ),
            (json!({"path": "a", "mode": "c"}), "'mode' must be one of"),
            (json!(["a"]), "takes keyword arguments"),
        ] {
            let why = checked("read", &read(), args).expect_err("refused");
            assert!(why.contains(said), "{why}");
        }
    }
}
