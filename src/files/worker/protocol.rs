use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind, Result};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub path: String,
    #[serde(default)]
    pub operation: Operation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<Guard>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    #[default]
    Observe,
    Presence,
    Read {
        offset: usize,
        limit: usize,
    },
    Replace {
        content: String,
    },
    Edit {
        old_text: String,
        new_text: String,
    },
    Create {
        content: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    pub revision_contract: u16,
    pub expected_revision: String,
    pub mutation_id: String,
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        if self.path.is_empty()
            || self.path.len() > 4096
            || self.path.as_bytes().contains(&0)
            || self.path.ends_with('/')
        {
            return Err(invalid("native file path must be one bounded file path"));
        }
        if let Operation::Read { offset, limit } = self.operation
            && (offset == 0 || limit == 0 || limit > 5000)
        {
            return Err(invalid("native read range is outside its bounds"));
        }
        if matches!(
            self.operation,
            Operation::Observe | Operation::Presence | Operation::Read { .. }
        ) {
            return if self.guard.is_none() {
                Ok(())
            } else {
                Err(invalid("observation cannot carry a mutation guard"))
            };
        }
        let guard = self
            .guard
            .as_ref()
            .ok_or_else(|| invalid("native mutation needs a complete guard"))?;
        if guard.revision_contract != 1
            || guard.mutation_id.is_empty()
            || guard.mutation_id.len() > 128
            || !guard
                .mutation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid("unsupported native mutation guard or identity"));
        }
        if matches!(self.operation, Operation::Create { .. }) {
            if guard.expected_revision != "absent" {
                return Err(invalid("creation requires an absent precondition"));
            }
        } else {
            revision(&guard.expected_revision)?;
        }
        match &self.operation {
            Operation::Replace { content } | Operation::Create { content } => text(content),
            Operation::Edit { old_text, new_text } => {
                text(old_text)?;
                text(new_text)?;
                if old_text.is_empty() {
                    return Err(invalid("native edit requires nonempty exact old_text"));
                }
                Ok(())
            }
            Operation::Observe | Operation::Presence | Operation::Read { .. } => Ok(()),
        }
    }

    pub(super) fn content(&self, source: Option<&str>) -> Result<String> {
        let value = match &self.operation {
            Operation::Replace { content } | Operation::Create { content } => content.clone(),
            Operation::Edit { old_text, new_text } => {
                let source =
                    source.ok_or_else(|| invalid("native edit needs an existing snapshot"))?;
                let first = source
                    .find(old_text)
                    .ok_or_else(|| invalid("old_text does not match the complete source"))?;
                if source.rfind(old_text) != Some(first) {
                    return Err(invalid("old_text is not unique in the complete source"));
                }
                source.replacen(old_text, new_text, 1)
            }
            Operation::Observe | Operation::Presence | Operation::Read { .. } => {
                return Err(invalid("observation is not a mutation"));
            }
        };
        text(&value)?;
        Ok(value)
    }
}

pub(super) fn revision(value: &str) -> Result<()> {
    let digest = value
        .strip_prefix("cfr1:")
        .ok_or_else(|| invalid("unsupported native revision algorithm"))?;
    crate::files::locking::Key::from_wire(digest).map(|_| ())
}

fn text(value: &str) -> Result<()> {
    if value.len() >= crate::source::EDIT_BYTES || value.as_bytes().contains(&0) {
        return Err(invalid(
            "native mutation text must be UTF-8 without NUL and smaller than 1 MiB",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
