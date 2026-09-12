//! Length-prefixed frames on casper's socket: a 4-byte big-endian length, then a JSON or CBOR body
//! sniffed from its first byte, the family's convention. A reply goes back in the call's encoding.
//! Duplicated from the siblings, not shared: a library between these programs is the dependency the
//! family exists to avoid.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{Read, Write};

/// The most one frame may carry, so a bad length cannot ask for a gigabyte before a byte is read.
pub const MOST: usize = 1 << 20;

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

/// Read one frame's body: the length, then that many bytes. `UnexpectedEof` on a clean close, and
/// a refusal before allocating when the length is over [`MOST`].
pub fn read_frame<R: Read>(from: &mut R) -> std::io::Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    from.read_exact(&mut header)?;
    let want = u32::from_be_bytes(header) as usize;
    if want > MOST {
        return Err(std::io::Error::other("that is too much to say at once"));
    }
    let mut body = vec![0_u8; want];
    from.read_exact(&mut body)?;
    Ok(body)
}

/// Write one body as a frame: its length, then it.
pub fn write_frame<W: Write>(to: &mut W, body: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| std::io::Error::other("frame too long"))?;
    to.write_all(&len.to_be_bytes())?;
    to.write_all(body)?;
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
    fn a_length_over_the_cap_is_refused_before_a_byte_is_read() {
        let header = (MOST as u32 + 1).to_be_bytes();
        let err = read_frame(&mut header.as_slice()).expect_err("refused");
        assert!(err.to_string().contains("too much"), "{err}");
    }
}
