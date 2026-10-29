use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{aead, rand::SecureRandom};
use std::sync::atomic::{AtomicU64, Ordering};

const PREFIX: &str = "mcp1.";
const NONCE: usize = 12;
const TAG: usize = 16;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Session,
    Cursor,
}

impl Kind {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::Session => b"casper-mcp-id-v1/session",
            Self::Cursor => b"casper-mcp-id-v1/cursor",
        }
    }

    fn empty(self, value: &str) -> bool {
        matches!(self, Self::Cursor) && value.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("authenticated MCP identifier refused")]
pub(super) struct Invalid;

pub(super) struct Cipher {
    key: aead::LessSafeKey,
    next: AtomicU64,
    cap: usize,
    encoded_cap: usize,
}

impl Cipher {
    pub(super) fn new(cap: usize) -> Result<Self, Invalid> {
        let cap = cap.min(super::super::REPLY);
        let encoded_cap =
            base64::encoded_len(cap + NONCE + TAG, false).ok_or(Invalid)? + PREFIX.len();
        let mut key = [0; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| Invalid)?;
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key).map_err(|_| Invalid)?;
        Ok(Self {
            key: aead::LessSafeKey::new(key),
            next: AtomicU64::new(0),
            cap,
            encoded_cap,
        })
    }

    pub(super) fn seal(&self, kind: Kind, value: &str) -> Result<String, Invalid> {
        if value.len() > self.cap {
            return Err(Invalid);
        }
        if kind.empty(value) {
            return Ok(String::new());
        }
        let sequence = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Invalid)?;
        let mut nonce = [0; NONCE];
        nonce
            .get_mut(4..)
            .ok_or(Invalid)?
            .copy_from_slice(&sequence.to_be_bytes());
        let mut encrypted = value.as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(kind.domain()),
                &mut encrypted,
            )
            .map_err(|_| Invalid)?;
        let mut bytes = Vec::with_capacity(NONCE + encrypted.len());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&encrypted);
        Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
    }

    pub(super) fn open(&self, kind: Kind, value: &str) -> Result<String, Invalid> {
        if value.len() > self.encoded_cap {
            return Err(Invalid);
        }
        if kind.empty(value) {
            return Ok(String::new());
        }
        let mut bytes = URL_SAFE_NO_PAD
            .decode(value.strip_prefix(PREFIX).ok_or(Invalid)?)
            .map_err(|_| Invalid)?;
        if !(NONCE + TAG..=NONCE + TAG + self.cap).contains(&bytes.len()) {
            return Err(Invalid);
        }
        let nonce = aead::Nonce::try_assume_unique_for_key(bytes.get(..NONCE).ok_or(Invalid)?)
            .map_err(|_| Invalid)?;
        let plain = self
            .key
            .open_in_place(
                nonce,
                aead::Aad::from(kind.domain()),
                bytes.get_mut(NONCE..).ok_or(Invalid)?,
            )
            .map_err(|_| Invalid)?;
        std::str::from_utf8(plain)
            .map(str::to_owned)
            .map_err(|_| Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_authenticated_bound_and_recoverable_without_eviction() {
        let cipher = Cipher::new(4096).expect("cipher");
        let other = Cipher::new(4096).expect("other transport");
        let old = cipher
            .seal(Kind::Cursor, "cursor-secret-canary")
            .expect("seal");
        for _ in 0..1024 {
            let sealed = cipher
                .seal(Kind::Cursor, "cursor-secret-canary")
                .expect("seal");
            assert_ne!(sealed, old);
            assert!(!sealed.contains("secret-canary"));
            assert_eq!(
                cipher.open(Kind::Cursor, &sealed).expect("open"),
                "cursor-secret-canary"
            );
        }
        assert_eq!(
            cipher
                .open(Kind::Cursor, &old)
                .expect("old cursor retained"),
            "cursor-secret-canary"
        );
        assert!(other.open(Kind::Cursor, &old).is_err());
        assert!(cipher.open(Kind::Session, &old).is_err());
        let mut bytes = URL_SAFE_NO_PAD
            .decode(old.strip_prefix(PREFIX).expect("prefix"))
            .expect("bytes");
        *bytes.last_mut().expect("tag") ^= 1;
        assert!(
            cipher
                .open(
                    Kind::Cursor,
                    &format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
                )
                .is_err()
        );
        assert!(
            cipher
                .open(Kind::Cursor, "unwrapped-secret-canary")
                .is_err()
        );
        assert_eq!(cipher.seal(Kind::Cursor, "").expect("reset"), "");
        assert_eq!(cipher.open(Kind::Cursor, "").expect("reset"), "");
        let empty_session = cipher.seal(Kind::Session, "").expect("session");
        assert!(!empty_session.is_empty());
        assert_eq!(
            cipher.open(Kind::Session, &empty_session).expect("session"),
            ""
        );
    }

    #[test]
    fn identifier_limits_and_nonce_exhaustion_fail_closed() {
        let cipher = Cipher::new(32).expect("cipher");
        let exact = cipher.seal(Kind::Session, &"x".repeat(32)).expect("bound");
        assert_eq!(
            cipher.open(Kind::Session, &exact).expect("open"),
            "x".repeat(32)
        );
        assert!(cipher.seal(Kind::Session, &"x".repeat(33)).is_err());
        assert!(
            cipher
                .open(Kind::Session, &"x".repeat(cipher.encoded_cap + 1))
                .is_err()
        );
        for invalid in ["", "mcp1.", "mcp1.%%%%", "mcp1.YQ"] {
            assert!(cipher.open(Kind::Session, invalid).is_err());
        }
        cipher.next.store(u64::MAX, Ordering::Relaxed);
        assert!(cipher.seal(Kind::Session, "next").is_err());
        assert_eq!(cipher.next.load(Ordering::Relaxed), u64::MAX);
        assert!(cipher.open(Kind::Session, &exact).is_ok());
    }

    #[test]
    fn concurrent_streams_share_a_non_repeating_nonce_sequence() {
        let cipher = std::sync::Arc::new(Cipher::new(4096).expect("cipher"));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cipher = cipher.clone();
                std::thread::spawn(move || {
                    (0..64)
                        .map(|_| cipher.seal(Kind::Cursor, "shared-cursor").expect("seal"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut nonces = std::collections::BTreeSet::new();
        for thread in threads {
            for sealed in thread.join().expect("stream") {
                assert_eq!(
                    cipher.open(Kind::Cursor, &sealed).expect("roundtrip"),
                    "shared-cursor"
                );
                let bytes = URL_SAFE_NO_PAD
                    .decode(sealed.strip_prefix(PREFIX).expect("prefix"))
                    .expect("frame");
                assert!(nonces.insert(bytes.get(..NONCE).expect("nonce").to_vec()));
            }
        }
        assert_eq!(nonces.len(), 512);
    }
}
