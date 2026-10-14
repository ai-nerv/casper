//! Length-prefixed frames on casper's socket: a 4-byte big-endian length, then a JSON or CBOR body
//! sniffed from its first byte, the family's convention. A reply goes back in the call's encoding.
//! Duplicated from the siblings, not shared: a library between these programs is the dependency the
//! family exists to avoid.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// The most one frame may carry, in either direction: the family's one cap.
pub const MOST: usize = 16 * 1024 * 1024;

/// How much of a body is read at a time, so memory follows what has arrived.
const CHUNK: usize = 64 * 1024;

/// Which encoding a body is in.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Wire {
    #[default]
    Json,
    Cbor,
}

impl Wire {
    /// Sniffed from the first non-whitespace byte: a CBOR map or array, else JSON.
    #[must_use]
    pub fn of(body: &[u8]) -> Self {
        match body.iter().find(|b| !b.is_ascii_whitespace()) {
            Some(0x80..=0xBF) => Self::Cbor,
            _ => Self::Json,
        }
    }

    /// One value as bytes in this encoding.
    pub fn encode<T: Serialize>(self, value: &T) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Json => serde_json::to_vec(value).map_err(std::io::Error::other),
            Self::Cbor => {
                let mut bytes = Vec::new();
                ciborium::into_writer(value, &mut bytes).map_err(std::io::Error::other)?;
                Ok(bytes)
            }
        }
    }
}

/// One value out of a body, in whichever encoding it is in.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> std::io::Result<T> {
    match Wire::of(body) {
        Wire::Json => serde_json::from_slice(body).map_err(std::io::Error::other),
        Wire::Cbor => ciborium::from_reader(body).map_err(std::io::Error::other),
    }
}

/// What the cap says about a frame of `len` bytes: `FileTooLarge` over it, `InvalidData` at zero.
fn sized(len: usize) -> std::io::Result<()> {
    if len > MOST {
        return Err(std::io::Error::new(
            ErrorKind::FileTooLarge,
            format!("a frame of {len} bytes is over the family's cap of {MOST}"),
        ));
    }
    if len == 0 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "a frame of length zero is malformed",
        ));
    }
    Ok(())
}

/// Read one frame's body: the length, then that many bytes. `UnexpectedEof` on a clean close,
/// `FileTooLarge` before allocating when the length is over [`MOST`], `InvalidData` at zero.
pub fn read_frame<R: Read>(from: &mut R) -> std::io::Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    from.read_exact(&mut header)?;
    let want = u32::from_be_bytes(header) as usize;
    sized(want)?;
    let mut body = vec![0_u8; want];
    from.read_exact(&mut body)?;
    Ok(body)
}

/// Write one body as a frame: its length, then it. Refused before a byte is sent when it is
/// empty or over [`MOST`].
pub fn write_frame<W: Write>(to: &mut W, body: &[u8]) -> std::io::Result<()> {
    sized(body.len())?;
    let len = u32::try_from(body.len()).map_err(|_| std::io::Error::other("frame too long"))?;
    to.write_all(&len.to_be_bytes())?;
    to.write_all(body)?;
    to.flush()
}

/// A socket a frame can be read from and written to by a deadline.
pub trait Timed: Read + Write {
    /// Set how long the next read may block.
    fn read_by(&self, at: Duration) -> std::io::Result<()>;
    /// Set how long the next write may block.
    fn write_by(&self, at: Duration) -> std::io::Result<()>;
}

impl Timed for std::os::unix::net::UnixStream {
    fn read_by(&self, at: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(at.max(Duration::from_millis(1))))
    }

    fn write_by(&self, at: Duration) -> std::io::Result<()> {
        self.set_write_timeout(Some(at.max(Duration::from_millis(1))))
    }
}

/// The time left before `deadline`, or `TimedOut` once it has passed.
fn left(deadline: Instant) -> std::io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| std::io::Error::new(ErrorKind::TimedOut, "the deadline passed"))
}

/// A socket timeout, which surfaces as `WouldBlock`, named as the deadline it is.
fn timed(why: std::io::Error) -> std::io::Error {
    if why.kind() == ErrorKind::WouldBlock {
        return std::io::Error::new(ErrorKind::TimedOut, "the deadline passed");
    }
    why
}

/// Read one frame from a peer that may wait `idle` for its first byte and then has `whole` to
/// send the rest, however it trickles. Memory grows with what arrives, not with what is claimed.
pub fn read_frame_by<S: Timed>(
    from: &mut S,
    idle: Duration,
    whole: Duration,
) -> std::io::Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    from.read_by(idle)?;
    let first = loop {
        match from.read(&mut header[..1]) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(_) => break Instant::now() + whole,
            Err(why) if why.kind() == ErrorKind::Interrupted => {}
            Err(why) => return Err(timed(why)),
        }
    };
    filled(from, &mut header[1..], first)?;
    let want = u32::from_be_bytes(header) as usize;
    sized(want)?;
    let mut body = Vec::with_capacity(want.min(CHUNK));
    let mut chunk = vec![0_u8; want.min(CHUNK)];
    while body.len() < want {
        chunk.truncate(want - body.len());
        filled(from, &mut chunk, first)?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Fill `buffer` by `deadline`.
fn filled<S: Timed>(from: &mut S, buffer: &mut [u8], deadline: Instant) -> std::io::Result<()> {
    let mut at = 0;
    while let Some(rest) = buffer.get_mut(at..).filter(|rest| !rest.is_empty()) {
        from.read_by(left(deadline)?)?;
        match from.read(rest) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(read) => at += read,
            Err(why) if why.kind() == ErrorKind::Interrupted => {}
            Err(why) => return Err(timed(why)),
        }
    }
    Ok(())
}

/// Write one body as a frame, all of it within `whole`, however slowly the peer reads.
pub fn write_frame_by<S: Timed>(to: &mut S, body: &[u8], whole: Duration) -> std::io::Result<()> {
    sized(body.len())?;
    let len = u32::try_from(body.len()).map_err(|_| std::io::Error::other("frame too long"))?;
    let deadline = Instant::now() + whole;
    for part in [&len.to_be_bytes()[..], body] {
        let mut at = 0;
        while let Some(rest) = part.get(at..).filter(|rest| !rest.is_empty()) {
            to.write_by(left(deadline)?)?;
            match to.write(rest) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(wrote) => at += wrote,
                Err(why) if why.kind() == ErrorKind::Interrupted => {}
                Err(why) => return Err(timed(why)),
            }
        }
    }
    to.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_a_length_then_a_body_and_reads_back() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"hello").expect("write");
        assert_eq!(&buffer[..4], &5_u32.to_be_bytes());
        let body = read_frame(&mut buffer.as_slice()).expect("read");
        assert_eq!(body, b"hello");
    }

    #[test]
    fn a_body_encodes_and_decodes_in_either_encoding() {
        let value = serde_json::json!({"call": "tools", "args": []});
        for how in [Wire::Json, Wire::Cbor] {
            let body = how.encode(&value).expect("encode");
            assert_eq!(Wire::of(&body), how, "the sniff names the encoding back");
            let back: serde_json::Value = decode(&body).expect("decode");
            assert_eq!(back, value, "one shape, two encodings");
        }
    }

    #[test]
    fn the_cap_is_the_familys_sixteen_mebibytes() {
        assert_eq!(MOST, 16_777_216);
    }

    #[test]
    fn a_length_over_the_cap_is_refused_before_a_byte_is_read() {
        let header = (MOST as u32 + 1).to_be_bytes();
        let err = read_frame(&mut header.as_slice()).expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::FileTooLarge, "{err}");
        assert!(err.to_string().contains("16777216"), "{err}");
    }

    #[test]
    fn a_frame_at_the_cap_reads_and_one_past_it_is_never_written() {
        let body = vec![b'x'; MOST];
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &body).expect("at the cap");
        assert_eq!(
            read_frame(&mut buffer.as_slice()).expect("read").len(),
            MOST
        );
        let mut sent = Vec::new();
        let over = vec![b'x'; MOST + 1];
        let err = write_frame(&mut sent, &over).expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::FileTooLarge, "{err}");
        assert!(sent.is_empty(), "not a byte goes out");
    }

    #[test]
    fn a_frame_of_length_zero_is_malformed_both_ways() {
        let err = read_frame(&mut [0_u8; 4].as_slice()).expect_err("malformed");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        let mut sent = Vec::new();
        assert!(write_frame(&mut sent, b"").is_err() && sent.is_empty());
    }

    #[test]
    fn a_peer_that_trickles_is_cut_off_at_the_whole_frame_deadline() {
        use std::os::unix::net::UnixStream;
        let (mut ours, mut theirs) = UnixStream::pair().expect("pair");
        let trickle = std::thread::spawn(move || {
            let _ = theirs.write_all(&8_u32.to_be_bytes());
            for _ in 0..20 {
                if theirs.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let began = Instant::now();
        let idle = Duration::from_secs(5);
        let err = read_frame_by(&mut ours, idle, Duration::from_millis(150)).expect_err("cut");
        assert_eq!(err.kind(), ErrorKind::TimedOut, "{err}");
        assert!(began.elapsed() < Duration::from_secs(2));
        drop(ours);
        let _ = trickle.join();
    }

    #[test]
    fn an_idle_peer_is_cut_off_at_the_idle_deadline() {
        let (mut ours, _theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
        let idle = Duration::from_millis(100);
        let err = read_frame_by(&mut ours, idle, Duration::from_secs(5)).expect_err("idle");
        assert_eq!(err.kind(), ErrorKind::TimedOut, "{err}");
    }
}
