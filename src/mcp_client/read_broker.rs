use crate::tools::{ReadArgs, ReadData};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub const KEY: &str = "io.github.bresilla.casper/native-read";
pub const FLAG: &str = "--mcp-read-acquire";
pub const LIMIT: usize = 4_000_000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    version: u16,
    operation: String,
    root: PathBuf,
}

pub(crate) fn descriptor(root: &Path) -> Value {
    serde_json::json!({"version":1,"operation":"read","root":root})
}

pub(super) fn negotiated(value: Option<&Value>, root: &Path) -> Result<bool, String> {
    let Some(value) = value else { return Ok(false) };
    let descriptor: Descriptor = serde_json::from_value(value.clone())
        .map_err(|_| "invalid native read broker descriptor")?;
    if descriptor.version != 1 || descriptor.operation != "read" || descriptor.root != root {
        return Err("unsupported native read broker version, operation or launch root".into());
    }
    Ok(true)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    version: u16,
    sequence: u64,
    root: PathBuf,
    tool: String,
    args: Value,
    data: ReadData,
}

impl Envelope {
    pub(super) fn new(sequence: u64, root: &Path, tool: &str, args: Value, data: ReadData) -> Self {
        Self {
            version: 1,
            sequence,
            root: root.into(),
            tool: tool.into(),
            args,
            data,
        }
    }
}

#[derive(Default)]
pub(crate) struct Consumed {
    high: u64,
    seen: BTreeSet<u64>,
}

impl Consumed {
    pub(crate) fn take(
        &mut self,
        envelope: Envelope,
        root: &Path,
        tool: &str,
        args: &Value,
    ) -> Result<ReadData, String> {
        if envelope.version != 1
            || envelope.root != root
            || envelope.tool != tool
            || &envelope.args != args
        {
            return Err("native read broker data does not match this call".into());
        }
        ReadArgs::parse(args)?;
        if envelope.sequence == 0
            || envelope.sequence <= self.high.saturating_sub(1024)
            || !self.seen.insert(envelope.sequence)
        {
            return Err("native read broker sequence is stale or already consumed".into());
        }
        self.high = self.high.max(envelope.sequence);
        self.seen
            .retain(|sequence| *sequence > self.high.saturating_sub(1024));
        Ok(envelope.data)
    }
}

pub fn acquire_input(input: impl std::io::Read) -> Result<Value, String> {
    let mut bytes = Vec::new();
    input
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 {
        return Err("broker read arguments exceed their bound".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let args = ReadArgs::parse(&value)?;
    if !crate::jail::Jail::from_env().on() {
        return Err("broker acquisition requires containment".into());
    }
    let data = if let Some(image) = crate::images::jailed_file(&args.path)? {
        ReadData::Image(image)
    } else {
        let reading = crate::files::worker::read(
            &args.path,
            args.offset.unwrap_or(1),
            args.limit.unwrap_or(2000),
        )
        .map_err(|e| e.to_string())?;
        ReadData::Text(reading.presentation())
    };
    serde_json::to_value(data).map_err(|e| e.to_string())
}

pub(super) async fn acquire(root: &Path, args: &Value) -> Result<ReadData, String> {
    ReadArgs::parse(args)?;
    let mut command = crate::jail::worker(root).map_err(|e| e.to_string())?;
    if std::env::var_os(crate::jail::WANTED).is_none() {
        command.env(crate::jail::WANTED, "1");
    }
    let mut child = command
        .arg("code")
        .arg(FLAG)
        .arg("--json")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("broker acquisition stdin missing")?;
    stdin
        .write_all(args.to_string().as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.shutdown().await.map_err(|e| e.to_string())?;
    drop(stdin);
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or("broker acquisition stdout missing")?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT {
        return Err("broker read data exceeds the transport bound".into());
    }
    if !child.wait().await.map_err(|e| e.to_string())?.success() {
        return Err("broker acquisition failed".into());
    }
    let reply: crate::wire::Reply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| "broker acquisition refused".into()));
    }
    if reply.family != crate::wire::FAMILY
        || reply.n != 1
        || reply.result.len() != 1
        || reply.error.is_some()
        || reply.fault.is_some()
    {
        return Err("broker acquisition returned an invalid reply".into());
    }
    serde_json::from_value(
        reply
            .result
            .into_iter()
            .next()
            .ok_or("broker read data missing")?,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
