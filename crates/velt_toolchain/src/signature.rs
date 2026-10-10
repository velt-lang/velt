//! Release signatures: each release's `SHA256SUMS` is signed with the release key (Ed25519), and
//! `SHA256SUMS.sig` holds the signature. A hash from the same place as the archive only shows
//! the download is intact; checked against the signature, it also shows the archive is the one
//! the velt project published, whatever mirror (`$VELT_INSTALL_BASE_URL`) served it.
//!
//! The public key is built into velt ([`RELEASE_PUBLIC_KEY`]). `$VELT_INSTALL_PUBLIC_KEY` (hex)
//! replaces it, for releases of another build (a fork's, a test's): whoever can set your
//! environment can already run programs as you, so it opens nothing new.

use ring::signature::{UnparsedPublicKey, ED25519};

use crate::install::download;

/// The velt release key: an Ed25519 public key, hex. The private half signs releases in
/// `.github/workflows/release.yml` (secret `VELT_RELEASE_SIGNING_KEY`).
pub const RELEASE_PUBLIC_KEY: &str =
    "95704338078ff393e38d91fa0b35407bef91f2c6a23e01786cd60151006105cb";
/// Overrides [`RELEASE_PUBLIC_KEY`].
pub const ENV_PUBLIC_KEY: &str = "VELT_INSTALL_PUBLIC_KEY";
/// The signature of `SHA256SUMS`, beside it.
pub const SIG_FILE: &str = "SHA256SUMS.sig";

/// The key releases are checked with: `$VELT_INSTALL_PUBLIC_KEY`, else the built-in one.
pub fn public_key() -> Result<Vec<u8>, String> {
    let (hex, from) = match std::env::var(ENV_PUBLIC_KEY)
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        Some(k) => (k, format!("${ENV_PUBLIC_KEY}")),
        None => (
            RELEASE_PUBLIC_KEY.to_string(),
            "this velt's release key".into(),
        ),
    };
    if hex.trim().is_empty() {
        return Err(format!(
            "this velt has no release key built in, so it cannot check downloads; set \
             ${ENV_PUBLIC_KEY} to the key of the releases you trust"
        ));
    }
    match decode_hex(hex.trim()) {
        Some(key) if key.len() == 32 => Ok(key),
        _ => Err(format!(
            "{from} is not an Ed25519 public key (64 hex digits)"
        )),
    }
}

/// Check `signature` (64 bytes, or their hex) of `data` with `key`.
pub fn verify(data: &[u8], signature: &[u8], key: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(signature).ok().map(str::trim);
    let signature = match text.and_then(decode_hex) {
        Some(bytes) => bytes,
        None => signature.to_vec(),
    };
    UnparsedPublicKey::new(&ED25519, key)
        .verify(data, &signature)
        .map_err(|_| "the signature does not match the release key".to_string())
}

/// The `SHA256SUMS` of the release at `release_url`, checked against its signature with `key`
/// ([`public_key`]); `None` when the release has no `SHA256SUMS` (it does not exist).
pub fn signed_sums(release_url: &str, key: &[u8]) -> Result<Option<String>, String> {
    let Some(sums) = download(&format!("{release_url}/SHA256SUMS"))? else {
        return Ok(None);
    };
    let signature = download(&format!("{release_url}/{SIG_FILE}"))?.ok_or_else(|| {
        format!("{release_url} has no {SIG_FILE}: the release is not signed, so its files cannot be checked")
    })?;
    verify(&sums, &signature, key)
        .map_err(|e| format!("{release_url}/SHA256SUMS: {e}; refusing its files"))?;
    String::from_utf8(sums)
        .map(Some)
        .map_err(|_| format!("{release_url}/SHA256SUMS is not text"))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    /// A fresh key pair: (signer, public key in hex).
    pub(crate) fn key_pair() -> (Ed25519KeyPair, String) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = hex(pair.public_key().as_ref());
        (pair, public)
    }

    pub(crate) fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn signatures_in_hex_or_raw() {
        let (pair, public) = key_pair();
        let key = decode_hex(&public).unwrap();
        let sums = b"abc  velt-0.1.0-x.tar.gz\n";
        let sig = pair.sign(sums);
        verify(sums, sig.as_ref(), &key).unwrap();
        verify(sums, format!("{}\n", hex(sig.as_ref())).as_bytes(), &key).unwrap();
        let err = verify(b"abc  velt-0.1.0-y.tar.gz\n", sig.as_ref(), &key).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        let (other, _) = key_pair();
        assert!(verify(sums, other.sign(sums).as_ref(), &key).is_err());
        assert!(verify(sums, b"zz", &key).is_err());
    }

    #[test]
    fn the_built_in_key_is_an_ed25519_key() {
        let key = decode_hex(RELEASE_PUBLIC_KEY).unwrap();
        assert_eq!(key.len(), 32);
        // A signature made with another key does not pass with it.
        let (other, _) = key_pair();
        assert!(verify(b"x", other.sign(b"x").as_ref(), &key).is_err());
    }

    #[test]
    fn hex_digits() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("zz"), None);
        assert_eq!(decode_hex("é0"), None);
    }
}
