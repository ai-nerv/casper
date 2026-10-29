use super::{
    protocol::{Operation, Request},
    snapshot::{Resolved, Snapshot},
    staging::Stage,
};
use crate::files::{control::ReceivedLease, locking::Key};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind, Result};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub revision_contract: u16,
    pub mutation: Mutation,
    pub observation: Option<Snapshot>,
    pub warning: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutation {
    pub id: String,
    pub outcome: String,
    pub durability: String,
}

#[derive(Debug)]
pub enum Failure {
    Refused(Error),
    Unknown(Error),
}

impl Failure {
    pub fn outcome_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(error) => std::fmt::Display::fmt(error, formatter),
            Self::Unknown(error) => write!(
                formatter,
                "native mutation outcome unknown; reread required, do not replay: {error}"
            ),
        }
    }
}

impl std::error::Error for Failure {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    SnapshotVerified,
    Staged,
    BeforeCommit,
    Committed,
    DirectorySync,
}

pub(super) fn run(
    resolved: &Resolved,
    lease: &ReceivedLease<'_>,
    request: &Request,
) -> Result<Commit> {
    run_with(resolved, lease, request, |_| Ok(()))
}

pub(super) fn run_with(
    resolved: &Resolved,
    _lease: &ReceivedLease<'_>,
    request: &Request,
    mut edge: impl FnMut(Edge) -> Result<()>,
) -> Result<Commit> {
    request.validate()?;
    let guard = request
        .guard
        .as_ref()
        .ok_or_else(|| Error::other("native mutation guard missing"))?;
    let creation = matches!(request.operation, Operation::Create { .. });
    resolved.validate_parent()?;
    let source = if creation {
        absent(resolved)?;
        None
    } else {
        let acquired = resolved.acquire(true)?;
        if acquired.snapshot.revision != guard.expected_revision {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "native source revision is stale; reread before mutation",
            ));
        }
        Some(acquired)
    };
    let content = request.content(
        source
            .as_ref()
            .map(|value| value.snapshot.source_text.as_str()),
    )?;
    edge(Edge::SnapshotVerified)?;
    let mut stage = Stage::prepare(&resolved.parent, source.as_ref(), &content)?;
    edge(Edge::Staged)?;
    if !creation {
        stage.link_for_replace()?;
    }
    edge(Edge::BeforeCommit)?;
    resolved.validate_parent()?;
    if creation {
        absent(resolved)?;
    } else if resolved.capture()?.revision != guard.expected_revision {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "native source changed before commit; reread before mutation",
        ));
    }
    stage.commit(&resolved.leaf, creation)?;
    let reported = edge(Edge::Committed);
    let sync = edge(Edge::DirectorySync)
        .and_then(|_| rustix::fs::fsync(&resolved.parent).map_err(Error::from));
    let mut warnings = Vec::new();
    if let Err(error) = reported {
        warnings.push(format!("committed; postcommit reporting failed: {error}"));
    }
    if let Err(error) = &sync {
        warnings.push(format!(
            "committed but directory durability is unconfirmed: {error}"
        ));
    }
    let observation = match resolved.acquire(false) {
        Ok(value)
            if stage.matches(&value).unwrap_or(false) && value.snapshot.source_text == content =>
        {
            Some(value.snapshot)
        }
        _ => {
            warnings.push(
                "committed but a current observation could not be confirmed; reread required"
                    .into(),
            );
            None
        }
    };
    Ok(Commit {
        revision_contract: 1,
        mutation: Mutation {
            id: guard.mutation_id.clone(),
            outcome: "committed".into(),
            durability: if sync.is_ok() {
                "synced"
            } else {
                "unconfirmed"
            }
            .into(),
        },
        observation,
        warning: if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("; "))
        },
    })
}

fn absent(resolved: &Resolved) -> Result<()> {
    match rustix::fs::statat(
        &resolved.parent,
        resolved.leaf.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Ok(_) => Err(Error::new(
            ErrorKind::AlreadyExists,
            "native creation target already exists",
        )),
        Err(error) => Err(error.into()),
    }
}

impl Commit {
    pub(super) fn validate(&self, key: &Key, request: &Request) -> Result<()> {
        let guard = request
            .guard
            .as_ref()
            .ok_or_else(|| Error::other("mutation reply has no request guard"))?;
        if self.revision_contract != 1
            || self.mutation.id != guard.mutation_id
            || self.mutation.outcome != "committed"
            || !matches!(self.mutation.durability.as_str(), "synced" | "unconfirmed")
            || (self.observation.is_none() || self.mutation.durability == "unconfirmed")
                && self.warning.is_none()
            || self
                .warning
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 2048)
        {
            return Err(Error::other(
                "native mutation reply is incomplete or unsupported; outcome unknown, reread required",
            ));
        }
        if let Some(observation) = &self.observation {
            observation.validate(key)?;
            if let Operation::Replace { content } | Operation::Create { content } =
                &request.operation
                && observation.source_text != *content
            {
                return Err(Error::other(
                    "native commit observation does not match the requested content; reread required",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
