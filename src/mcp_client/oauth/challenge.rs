use super::Error;
use std::collections::BTreeMap;

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn value(input: &str) -> Result<String, Error> {
    if !input.starts_with('"') {
        return if token(input) {
            Ok(input.into())
        } else {
            Err(Error::Metadata)
        };
    }
    if input.len() < 2 || !input.ends_with('"') {
        return Err(Error::Metadata);
    }
    let mut escaped = false;
    let mut output = String::new();
    for byte in input
        .as_bytes()
        .get(1..input.len() - 1)
        .ok_or(Error::Metadata)?
        .iter()
        .copied()
    {
        if !(0x20..=0x7e).contains(&byte) || (!escaped && byte == b'"') {
            return Err(Error::Metadata);
        }
        if !escaped && byte == b'\\' {
            escaped = true;
            continue;
        }
        output.push(char::from(byte));
        escaped = false;
    }
    if escaped {
        return Err(Error::Metadata);
    }
    Ok(output)
}

pub(super) fn bearer(input: &str) -> Result<BTreeMap<String, String>, Error> {
    if input.len() > 8192 || input.bytes().any(|b| !(0x20..0x7f).contains(&b)) {
        return Err(Error::Metadata);
    }
    let (mut quoted, mut escaped, mut start) = (false, false, 0);
    let mut fields = Vec::new();
    for (i, byte) in input.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quoted = !quoted;
        }
        if byte == b',' && !quoted {
            fields.push(input.get(start..i).ok_or(Error::Metadata)?);
            start = i + 1;
        }
    }
    if quoted || escaped {
        return Err(Error::Metadata);
    }
    fields.push(input.get(start..).ok_or(Error::Metadata)?);
    let (mut active, mut seen) = (false, false);
    let mut result = BTreeMap::new();
    for part in fields {
        let mut part = part.trim();
        if part.is_empty() {
            return Err(Error::Metadata);
        }
        let prefix = part.split([' ', '=']).next().ok_or(Error::Metadata)?;
        let rest = part
            .get(prefix.len()..)
            .ok_or(Error::Metadata)?
            .trim_start();
        if !rest.starts_with('=') {
            if !token(prefix) {
                return Err(Error::Metadata);
            }
            active = prefix.eq_ignore_ascii_case("bearer");
            if active && seen {
                return Err(Error::Metadata);
            }
            seen |= active;
            part = rest;
            if part.is_empty() {
                continue;
            }
        }
        if !active {
            continue;
        }
        let (name, item) = part.split_once('=').ok_or(Error::Metadata)?;
        let name = name.trim();
        if !token(name)
            || result
                .insert(name.to_ascii_lowercase(), value(item.trim())?)
                .is_some()
        {
            return Err(Error::Metadata);
        }
    }
    Ok(result)
}
