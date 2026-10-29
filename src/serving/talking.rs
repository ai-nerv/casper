//! One connection on casper's socket: calls in, replies out, each by a deadline, with the key
//! checked on every call and nothing over the family's cap ever written.

use super::key::{Key, REFUSED};
use crate::framing::{self, MOST, Wire};
use crate::wire::{Call, FAMILY, Reply};
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// How long a connection may sit between calls.
pub const IDLE: Duration = Duration::from_secs(300);

/// How long the rest of a frame may take once its first byte has arrived, and a reply to go out.
pub const MID_FRAME: Duration = Duration::from_secs(30);

/// A call as it arrives on the socket: the family's call, the key beside it, and the revision it
/// says it is written in, when it says one.
#[derive(serde::Deserialize)]
struct Keyed {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    family: Option<u64>,
    #[serde(default)]
    event_context: Option<serde_json::Value>,
    #[serde(flatten)]
    call: Call,
}

/// A call from a newer revision of the family is refused by name, before anything in it is read.
fn newer(family: Option<u64>) -> Option<Reply> {
    let family = family.filter(|family| *family > u64::from(FAMILY))?;
    Some(Reply::refused(format!(
        "the call is written in family {family}, newer than the {FAMILY} casper reads"
    )))
}

/// Read calls and answer them until the peer closes, a deadline passes, or a frame cannot be
/// read and stay in step.
pub fn talk(
    mut stream: UnixStream,
    key: &Key,
    handle: &(dyn Fn(&Call) -> Reply + Sync),
) -> std::io::Result<()> {
    loop {
        let body = match framing::read_frame_by(&mut stream, IDLE, MID_FRAME) {
            Ok(body) => body,
            Err(why) if why.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            // A body that will not be read cannot be skipped: answer once, then close.
            Err(why) if why.kind() == ErrorKind::FileTooLarge => {
                return send(&mut stream, Wire::Json, &Reply::failed(why.to_string()));
            }
            Err(why) if why.kind() == ErrorKind::InvalidData => {
                let refused = Reply::refused(format!("that is not a call: {why}"));
                send(&mut stream, Wire::Json, &refused)?;
                continue;
            }
            Err(why) => return Err(why),
        };
        let wire = Wire::of(&body);
        let reply = match framing::decode::<Keyed>(&body) {
            Ok(keyed) => newer(keyed.family).unwrap_or_else(|| {
                if key.admits(keyed.key.as_deref()) {
                    crate::events::requests::with_peer(&stream, keyed.event_context, || {
                        super::answering(&stream, || handle(&keyed.call))
                    })
                } else {
                    Reply::refused(REFUSED)
                }
            }),
            Err(why) => Reply::refused(format!("that is not a call: {why}")),
        };
        send(&mut stream, wire, &reply)?;
    }
}

/// Write `reply`, or in its place a failure naming the cap when it would not fit in a frame.
pub fn send(stream: &mut UnixStream, wire: Wire, reply: &Reply) -> std::io::Result<()> {
    let mut body = wire.encode(reply)?;
    if body.len() > MOST {
        let over = format!(
            "the reply was {} bytes, over the family's cap of {MOST}",
            body.len()
        );
        body = wire.encode(&Reply::failed(over))?;
    }
    framing::write_frame_by(stream, &body, MID_FRAME)
}

/// Turn a connection away while every slot is taken: one refusal, then the close.
pub fn busy(mut stream: UnixStream, most: usize) {
    let said = Reply::failed(format!(
        "busy: casper is answering {most} connections already"
    ));
    if let Ok(body) = Wire::Json.encode(&said) {
        let _ = framing::write_frame_by(&mut stream, &body, Duration::from_secs(1));
    }
}
