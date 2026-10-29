use super::{Budget, Catalog, Failure, Root, Source, catalog_root, load_root, valid_name};
use crate::jail::{WORKFLOW_BINDING as BINDING, WorkflowBinding as Binding};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Component, PathBuf};

pub const INPUT_MOST: usize = 32 * 1024;
pub const REPLY_MOST: usize = 7 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub root: PathBuf,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Catalog {},
    Load { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u16,
    pub root: PathBuf,
    pub operation: Operation,
    pub result: Result<Payload, Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Payload {
    Catalog(Catalog),
    Source(Source),
}

impl Request {
    pub fn validate(&self) -> Result<(), Failure> {
        let path = self.root.to_str().ok_or(Failure::InvalidPath)?;
        if self.version != 1 {
            return Err(Failure::Protocol);
        }
        if !self.root.is_absolute()
            || path.len() > 4096
            || path.contains('\0')
            || self.root.components().count() > 256
            || self
                .root
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(Failure::InvalidPath);
        }
        if let Operation::Load { name } = &self.operation
            && !valid_name(name)
        {
            return Err(Failure::InvalidPath);
        }
        Ok(())
    }

    pub fn read(input: impl Read) -> Result<Self, Failure> {
        let mut bytes = Vec::new();
        input
            .take((INPUT_MOST + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::Protocol)?;
        if bytes.len() > INPUT_MOST {
            return Err(Failure::Protocol);
        }
        let request: Self = serde_json::from_slice(&bytes).map_err(|_| Failure::Protocol)?;
        request.validate()?;
        Ok(request)
    }

    pub fn response(&self, result: Result<Payload, Failure>) -> Response {
        Response {
            version: 1,
            root: self.root.clone(),
            operation: self.operation.clone(),
            result,
        }
    }
}

pub fn worker(request: &Request) -> Result<Payload, Failure> {
    request.validate()?;
    if std::env::var("CASPER_WORKFLOW_WORKER").ok().as_deref() != Some("1") {
        return Err(Failure::Worker);
    }
    let binding = std::env::var(BINDING).map_err(|_| Failure::Worker)?;
    if binding.len() > INPUT_MOST {
        return Err(Failure::Protocol);
    }
    let binding: Binding = serde_json::from_str(&binding).map_err(|_| Failure::Protocol)?;
    acquire(request, &binding)
}

fn acquire(request: &Request, binding: &Binding) -> Result<Payload, Failure> {
    if binding.root != request.root {
        return Err(Failure::Changed);
    }
    let budget = Budget::new();
    let root = Root::open(&request.root, &budget)?;
    if !binding.matches(root.path, &root.fd) {
        return Err(Failure::Changed);
    }
    match &request.operation {
        Operation::Catalog {} => catalog_root(&root, &budget).map(Payload::Catalog),
        Operation::Load { name } => load_root(&root, name, &budget).map(Payload::Source),
    }
}

impl Response {
    pub fn validate(&self, request: &Request) -> Result<(), Failure> {
        if self.version != 1 || self.root != request.root || self.operation != request.operation {
            return Err(Failure::Protocol);
        }
        match (&request.operation, &self.result) {
            (_, Err(_)) => Ok(()),
            (Operation::Catalog {}, Ok(Payload::Catalog(catalog))) => {
                if catalog.entries.len().saturating_add(catalog.invalid_names) > super::ENTRIES_MOST
                {
                    return Err(Failure::Protocol);
                }
                let mut previous: Option<&str> = None;
                for candidate in &catalog.entries {
                    if !valid_name(&candidate.name)
                        || previous.is_some_and(|p| p >= candidate.name.as_str())
                    {
                        return Err(Failure::Protocol);
                    }
                    if candidate
                        .metadata
                        .as_ref()
                        .is_ok_and(|text| text.len() > super::METADATA_MOST || text.contains('\0'))
                    {
                        return Err(Failure::Protocol);
                    }
                    previous = Some(&candidate.name);
                }
                Ok(())
            }
            (Operation::Load { .. }, Ok(Payload::Source(source))) => {
                use sha2::{Digest, Sha256};
                if source.text.len() > super::FILE_MOST
                    || source.text.contains('\0')
                    || source.sha256 != format!("{:x}", Sha256::digest(source.text.as_bytes()))
                {
                    return Err(Failure::Protocol);
                }
                Ok(())
            }
            _ => Err(Failure::Protocol),
        }
    }
}

#[cfg(test)]
mod tests;
