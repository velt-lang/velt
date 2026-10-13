//! The two keys of the WebSocket opening handshake (RFC 6455 §4): the client's random
//! `Sec-WebSocket-Key` and the server's `Sec-WebSocket-Accept`, derived from it.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha1::{Digest, Sha1};

/// The GUID RFC 6455 appends to the key before hashing.
const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A fresh `Sec-WebSocket-Key`: 16 random bytes, base64-encoded.
pub(super) fn generate_key() -> String {
    let mut nonce = [0u8; 16];
    // The key only has to differ between handshakes (it is no secret); without OS randomness,
    // fall back to the clock so a connect still works.
    if getrandom::fill(&mut nonce).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        nonce = t.to_le_bytes();
    }
    STANDARD.encode(nonce)
}

/// The `Sec-WebSocket-Accept` for a `Sec-WebSocket-Key`: base64 of SHA-1(key ‖ GUID).
pub(super) fn accept_key(key: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(key);
    h.update(GUID);
    STANDARD.encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_rfc_6455_example() {
        // RFC 6455 §1.3.
        assert_eq!(
            accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn generated_keys_are_16_bytes_and_differ() {
        let (a, b) = (generate_key(), generate_key());
        assert_eq!(STANDARD.decode(&a).unwrap().len(), 16);
        assert_ne!(a, b);
    }
}
