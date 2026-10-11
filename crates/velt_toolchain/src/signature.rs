//! Release signatures: each release's `SHA256SUMS`, and the index of releases
//! ([`crate::release`]), are signed with the release key (Ed25519); `<file>.sig` holds the
//! signature. A hash from the same place as the archive only shows
//! the download is intact; checked against the signature, it also shows the archive is the one
//! the velt project published, whatever mirror (`$VELT_INSTALL_BASE_URL`) served it.
//!
//! The public keys are built into velt ([`RELEASE_PUBLIC_KEYS`]): the one releases are signed
//! with, and a backup whose private half is kept offline. A signature by either is accepted, so
//! if the signing key is lost or leaks, releases are signed with the backup and the launchers
//! that carry it (those from the release after 0.2.0 on; 0.2.0's knows only the signing key)
//! keep checking them; the next release then names a new backup (CONTRIBUTING.md "Replacing the
//! signing key", which also says what this can't do: a leaked key can't be revoked from
//! launchers that trust it). `$VELT_INSTALL_PUBLIC_KEY` (hex, several separated by commas)
//! replaces them, for releases of another build (a fork's, a test's): whoever can set your
//! environment can already run programs as you, so it opens nothing new.

use ring::signature::{UnparsedPublicKey, ED25519};

use crate::install::{download, MAX_SMALL_FILE};

/// The velt release keys: Ed25519 public keys, hex, one per line (scripts/release-sign.sh and a
/// test read them from here). The first signs releases in `.github/workflows/release.yml`
/// (secret `VELT_RELEASE_SIGNING_KEY` of the `release` environment); the second is the backup,
/// its private half offline.
pub const RELEASE_PUBLIC_KEYS: &[&str] = &[
    "95704338078ff393e38d91fa0b35407bef91f2c6a23e01786cd60151006105cb",
    "84bb2873cee95e478045ff2195d7f8edb84fde21eebd7b96e00482b5207264be",
];
/// A signature by the backup key (`RELEASE_PUBLIC_KEYS[1]`) over [`BACKUP_KEY_PROOF_MESSAGE`],
/// made with its offline private half when the key was added: a test checks it, so a typo in
/// the public key, or a lost private half, shows now rather than when the backup is needed.
/// A new backup key comes with a new proof (CONTRIBUTING.md "Replacing the signing key").
pub const BACKUP_KEY_PROOF: &str = "a6ac0647dfa8622be9b6d2a5bd84dda612d7345401492217aabc60b860078dd8\
                                    18dd0929af7bb9428adeb3c6d0d61848d935cb1a8c5a07b51e649be7916aa109";
/// What [`BACKUP_KEY_PROOF`] signs: the text, then the backup key in hex.
pub const BACKUP_KEY_PROOF_MESSAGE: &str = "velt backup release key ";
/// Overrides [`RELEASE_PUBLIC_KEYS`].
pub const ENV_PUBLIC_KEY: &str = "VELT_INSTALL_PUBLIC_KEY";
/// A signed file's signature is beside it, named `<file>.sig` (`SHA256SUMS.sig`,
/// `releases.json.sig`): the 64-byte Ed25519 signature of the file's bytes, raw or in hex.
pub const SIG_SUFFIX: &str = ".sig";

/// The keys releases are checked with, the signing key first: `$VELT_INSTALL_PUBLIC_KEY`, else
/// the built-in ones.
pub fn public_keys() -> Result<Vec<Vec<u8>>, String> {
    match std::env::var(ENV_PUBLIC_KEY)
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        Some(list) => parse_keys(&list, &format!("${ENV_PUBLIC_KEY}")),
        None => parse_keys(&RELEASE_PUBLIC_KEYS.join(","), "this velt's release keys"),
    }
}

/// Ed25519 public keys in hex, separated by commas (or spaces).
fn parse_keys(list: &str, from: &str) -> Result<Vec<Vec<u8>>, String> {
    let keys = list
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|k| !k.is_empty())
        .map(|k| match decode_hex(k) {
            Some(key) if key.len() == 32 => Ok(key),
            _ => Err(format!(
                "{from}: `{k}` is not an Ed25519 public key (64 hex digits)"
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        return Err(format!(
            "{from} names no key, so downloads cannot be checked; set ${ENV_PUBLIC_KEY} to the \
             key of the releases you trust"
        ));
    }
    Ok(keys)
}

/// Check `signature` (64 bytes, or their hex) of `data`: it must be by one of `keys`.
pub fn verify(data: &[u8], signature: &[u8], keys: &[Vec<u8>]) -> Result<(), String> {
    let text = std::str::from_utf8(signature).ok().map(str::trim);
    let signature = match text.and_then(decode_hex) {
        Some(bytes) => bytes,
        None => signature.to_vec(),
    };
    if keys.iter().any(|key| {
        UnparsedPublicKey::new(&ED25519, key)
            .verify(data, &signature)
            .is_ok()
    }) {
        Ok(())
    } else {
        Err("the signature does not match the release key".to_string())
    }
}

/// The `SHA256SUMS` of the release at `release_url`, checked against its signature with `keys`
/// ([`public_keys`]); `None` when the release has no `SHA256SUMS` (it does not exist).
pub fn signed_sums(release_url: &str, keys: &[Vec<u8>]) -> Result<Option<String>, String> {
    signed_text(&format!("{release_url}/SHA256SUMS"), keys)
}

/// The text at `url`, checked against its signature at `<url>.sig` with `keys`; `None` when
/// `url` does not exist.
pub fn signed_text(url: &str, keys: &[Vec<u8>]) -> Result<Option<String>, String> {
    let Some(data) = download(url, MAX_SMALL_FILE)? else {
        return Ok(None);
    };
    let signature = download(&format!("{url}{SIG_SUFFIX}"), MAX_SMALL_FILE)?.ok_or_else(|| {
        format!("{url} has no signature ({url}{SIG_SUFFIX}), so it cannot be checked; refusing it")
    })?;
    verify(&data, &signature, keys).map_err(|e| format!("{url}: {e}; refusing it"))?;
    String::from_utf8(data)
        .map(Some)
        .map_err(|_| format!("{url} is not text"))
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
        let keys = vec![decode_hex(&public).unwrap()];
        let sums = b"abc  velt-0.1.0-x.tar.gz\n";
        let sig = pair.sign(sums);
        verify(sums, sig.as_ref(), &keys).unwrap();
        verify(sums, format!("{}\n", hex(sig.as_ref())).as_bytes(), &keys).unwrap();
        let err = verify(b"abc  velt-0.1.0-y.tar.gz\n", sig.as_ref(), &keys).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        let (other, _) = key_pair();
        assert!(verify(sums, other.sign(sums).as_ref(), &keys).is_err());
        assert!(verify(sums, b"zz", &keys).is_err());
        assert!(verify(sums, sig.as_ref(), &[]).is_err());
    }

    #[test]
    fn a_signature_by_any_trusted_key_passes() {
        let (signing, signing_public) = key_pair();
        let (backup, backup_public) = key_pair();
        let (stranger, _) = key_pair();
        let keys = parse_keys(&format!("{signing_public}, {backup_public}"), "test").unwrap();
        assert_eq!(keys.len(), 2);
        let data = b"SHA256SUMS";
        verify(data, signing.sign(data).as_ref(), &keys).unwrap();
        verify(data, backup.sign(data).as_ref(), &keys).unwrap();
        assert!(verify(data, stranger.sign(data).as_ref(), &keys).is_err());
    }

    #[test]
    fn key_lists() {
        let (_, a) = key_pair();
        assert_eq!(parse_keys(&format!("  {a} "), "x").unwrap().len(), 1);
        let err = parse_keys(&format!("{a},abc"), "$X").unwrap_err();
        assert!(
            err.starts_with("$X: `abc` is not an Ed25519 public key"),
            "{err}"
        );
        assert!(parse_keys(" , ", "x").unwrap_err().contains("names no key"));
    }

    #[test]
    fn the_built_in_keys_are_two_ed25519_keys() {
        assert_eq!(RELEASE_PUBLIC_KEYS.len(), 2, "the signing key and a backup");
        let keys = parse_keys(&RELEASE_PUBLIC_KEYS.join(","), "built in").unwrap();
        assert_ne!(keys[0], keys[1]);
        // A signature made with another key does not pass with them.
        let (other, _) = key_pair();
        assert!(verify(b"x", other.sign(b"x").as_ref(), &keys).is_err());
    }

    #[test]
    fn hex_digits() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("zz"), None);
        assert_eq!(decode_hex("é0"), None);
    }

    #[test]
    fn the_backup_key_has_signed_its_proof() {
        let keys = parse_keys(&RELEASE_PUBLIC_KEYS.join(","), "built in").unwrap();
        let message = format!("{BACKUP_KEY_PROOF_MESSAGE}{}", RELEASE_PUBLIC_KEYS[1]);
        let proof = BACKUP_KEY_PROOF.as_bytes();
        verify(message.as_bytes(), proof, &keys[1..]).unwrap();
        assert!(
            verify(message.as_bytes(), proof, &keys[..1]).is_err(),
            "the proof is the backup key's, not the signing key's"
        );
        assert!(verify(b"another message", proof, &keys).is_err());
    }
}
