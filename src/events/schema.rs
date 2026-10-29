use serde::{Deserialize, Serialize};
use std::{io::Read, num::NonZeroU64};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id([u8; 16]);

impl Id {
    pub(super) fn local(producer: Self, counter: u64) -> Self {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"casper-local-event-identity-v1");
        hash.update(producer.0);
        hash.update(counter.to_be_bytes());
        let mut bytes = [0; 16];
        for (slot, byte) in bytes.iter_mut().zip(hash.finalize()) {
            *slot = byte;
        }
        Self(bytes)
    }

    pub fn generate() -> std::io::Result<Self> {
        let mut bytes = [0; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Id {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid opaque event identity");
        }
        let mut bytes = [0; 16];
        for (slot, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            let digit = |byte: u8| {
                if byte <= b'9' {
                    byte - b'0'
                } else {
                    byte - b'a' + 10
                }
            };
            let [high, low] = pair else {
                return Err("invalid opaque event identity");
            };
            *slot = (digit(*high) << 4) | digit(*low);
        }
        Ok(Self(bytes))
    }
}

impl From<Id> for String {
    fn from(id: Id) -> Self {
        id.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Revision;

impl TryFrom<u8> for Revision {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err("unsupported event revision")
        }
    }
}

impl From<Revision> for u8 {
    fn from(_: Revision) -> Self {
        1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Program {
    Casper,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correlation {
    pub run_id: Option<Id>,
    pub agent_id: Option<Id>,
    pub turn_id: Option<Id>,
    pub request_id: Option<Id>,
    pub ask_id: Option<Id>,
    pub attempt_id: Option<Id>,
    pub job_id: Option<Id>,
    pub tool_call_id: Option<Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Admitted,
    Started,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    Refused,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    Confirmed,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    Confirmed,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Reported,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationState {
    Committed {
        durability: Durability,
        observation: Observation,
        delivery: Delivery,
    },
    Refused {},
    Unknown {},
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerOutcome {
    Connecting,
    Connected,
    CloseRequested,
    Disconnected,
    AuthenticationRequired,
    Refused,
    Failed,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fact {
    Tool {
        outcome: ToolOutcome,
        duration_ms: Option<u64>,
    },
    Mutation {
        state: MutationState,
    },
    ToolServer {
        outcome: ServerOutcome,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema: Revision,
    pub program: Program,
    pub program_version: [u16; 3],
    pub producer: Id,
    pub sequence: NonZeroU64,
    pub correlation: Correlation,
    pub fact: Fact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Oversized,
    Invalid,
}

pub fn decode(bytes: &[u8]) -> Result<Event, DecodeError> {
    if bytes.len() > super::MAX_EVENT_BYTES {
        return Err(DecodeError::Oversized);
    }
    serde_json::from_slice(bytes).map_err(|_| DecodeError::Invalid)
}

pub fn elapsed(start: std::time::Instant) -> Option<u64> {
    start.elapsed().as_millis().try_into().ok()
}
