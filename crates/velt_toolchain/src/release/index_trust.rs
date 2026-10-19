//! Trusting the index of releases: the revocations it carries, and its freshness
//! ([`super`] "The index of releases").

use super::*;

/// Now, in Unix seconds.
pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How far ahead of this machine's clock an index's `generated` may be: a day, for clocks that
/// are off. Later than that, it is refused and not remembered, or one dated years ahead (by a
/// leaked key) would make every later index look older.
pub const MAX_INDEX_AHEAD: u64 = 24 * 60 * 60;

/// Revoke the keys of `revoked` that come before `signer` in `trust.all` (the backup revokes
/// the signing key; a key never revokes one after it, so a leaked signing key can't revoke the
/// backup). Only the official keys revoke: `revoked-keys` is shared by every series, and
/// another build's keys (`$VELT_INSTALL_PUBLIC_KEY`) must not withdraw them. Keys this launcher
/// doesn't trust are not its concern. Returns what to tell the user.
pub(super) fn apply_revocations(
    root: &Root,
    trust: &Trust,
    signer: &[u8],
    revoked: &[Vec<u8>],
) -> Result<Vec<String>, String> {
    let mut notes = vec![];
    if trust.series != OFFICIAL_SERIES {
        return Ok(notes);
    }
    let Some(signer_at) = trust.all.iter().position(|k| k == signer) else {
        return Ok(notes);
    };
    for key in revoked {
        let Some(at) = trust.all.iter().position(|k| k == key) else {
            continue;
        };
        if at < signer_at && trust.keys.contains(key) {
            record_revoked(root, key)?;
            notes.push(format!(
                "release key {}… is revoked (the index of releases says so, signed with a later \
                 key, {}…); it is no longer trusted",
                &hex_of(key)[..8],
                &hex_of(signer)[..8]
            ));
        } else if at > signer_at {
            // A release never does this: either it is misconfigured, or the signing key leaked
            // and its holder is trying to revoke the backup.
            notes.push(format!(
                "warning: the index of releases, signed with release key {}…, revokes the later \
                 key {}…; ignored: only a later key may revoke an earlier one. Please report \
                 this to the velt project (https://github.com/velt-lang/velt/issues)",
                &hex_of(signer)[..8],
                &hex_of(key)[..8]
            ));
        }
    }
    Ok(notes)
}

/// Refuse an index older than the newest `root` has seen in `series` ([`Trust::series`]: the
/// official releases are one series whichever key signed them, another build's another), or
/// one written more than [`MAX_INDEX_AHEAD`] after `now`; remember a newer one. The official
/// series starts from what 0.2.0 kept ([`LEGACY_SERIES`]).
pub(super) fn remember_index(
    root: &Root,
    index: &Index,
    url: &str,
    series: &str,
    now: u64,
) -> Result<(), String> {
    if index.generated > now.saturating_add(MAX_INDEX_AHEAD) {
        return Err(format!(
            "{url} was written in the future (at {}, more than a day after this machine's clock, \
             {now}): check the clock; otherwise the mirror is serving a forged index",
            index.generated
        ));
    }
    let dir = root.dir().join(INDEX_SEEN_DIR);
    let read = |name: &str| -> Option<u64> {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    };
    let seen_file = dir.join(series);
    let seen: u64 = read(series)
        .or_else(|| {
            (series == OFFICIAL_SERIES)
                .then(|| read(LEGACY_SERIES))
                .flatten()
        })
        .unwrap_or(0);
    if index.generated < seen {
        return Err(format!(
            "{url} is older than an index seen before (written at {}, the newest seen at {seen}): \
             the mirror is behind, or is hiding newer releases; use an up-to-date one, or delete \
             {} to accept it",
            index.generated,
            seen_file.display()
        ));
    }
    if index.generated > seen {
        crate::layout::write_file(&seen_file, &format!("{}\n", index.generated))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::tests::serve;

    /// An index naming `revoked`, written at `generated` and signed by `by`, fetched from
    /// loopback by `root` trusting `all` in `series`.
    fn fetch_revoking(
        root: &Root,
        all: &[&str],
        series: &str,
        by: &ring::signature::Ed25519KeyPair,
        generated: u64,
        revoked: &[&str],
    ) -> Result<Index, String> {
        let path = "/releases/download/index/releases.json".to_string();
        let list: Vec<String> = revoked.iter().map(|h| format!("\"{h}\"")).collect();
        let text = format!(
            r#"{{"format": 1, "generated": {generated}, "revoked_keys": [{}], "releases": []}}"#,
            list.join(", ")
        );
        let server = serve(vec![
            (path.clone(), text.clone().into_bytes()),
            (
                format!("{path}.sig"),
                by.sign(text.as_bytes()).as_ref().to_vec(),
            ),
        ]);
        let all = all
            .iter()
            .map(|h| crate::signature::decode_hex(h).unwrap())
            .collect();
        let result = trust_from(all, series.into(), Some(root)).and_then(|t| {
            fetch_index_trusting(root, &format!("http://{}{path}", server.addr()), &t)
        });
        server.stop();
        result
    }

    #[test]
    fn the_backup_key_revokes_the_signing_key_for_good() {
        use crate::signature::revoked_keys;
        use crate::signature::tests::key_pair;
        let (signing, signing_hex) = key_pair();
        let (backup, backup_hex) = key_pair();
        let key = |h: &str| crate::signature::decode_hex(h).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        let keys = [signing_hex.as_str(), backup_hex.as_str()];
        let mut generated = 10;
        let mut fetch = |by: &ring::signature::Ed25519KeyPair, revoked: &[&str]| {
            generated += 1;
            fetch_revoking(&root, &keys, OFFICIAL_SERIES, by, generated, revoked)
        };
        // The signing key can't revoke the backup.
        fetch(&signing, &[&backup_hex]).unwrap();
        assert!(revoked_keys(&root).is_empty());
        // The backup revokes the signing key; kept even when a later index leaves it out.
        fetch(&backup, &[&signing_hex]).unwrap();
        assert_eq!(revoked_keys(&root), [key(&signing_hex)]);
        fetch(&backup, &[]).unwrap();
        assert_eq!(revoked_keys(&root), [key(&signing_hex)]);
        // From then on, what the revoked key signs is refused.
        let err = fetch(&signing, &[]).unwrap_err();
        assert!(err.contains("does not match the release key"), "{err}");
        // A key nobody trusts here is not this launcher's concern.
        let (_, stranger) = key_pair();
        fetch(&backup, &[&stranger]).unwrap();
        assert_eq!(revoked_keys(&root).len(), 1);
    }

    #[test]
    fn an_index_dated_ahead_by_a_leaked_key_does_not_block_its_revocation() {
        use crate::signature::revoked_keys;
        use crate::signature::tests::key_pair;
        let (signing, signing_hex) = key_pair();
        let (backup, backup_hex) = key_pair();
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        let keys = [signing_hex.as_str(), backup_hex.as_str()];
        let seen =
            || std::fs::read_to_string(root.dir().join(INDEX_SEEN_DIR).join(OFFICIAL_SERIES)).ok();
        fetch_revoking(&root, &keys, OFFICIAL_SERIES, &signing, 100, &[]).unwrap();
        // The leaked key dates an index far ahead: refused, and not remembered.
        let err = fetch_revoking(&root, &keys, OFFICIAL_SERIES, &signing, 99_999_999_999, &[])
            .unwrap_err();
        assert!(err.contains("written in the future"), "{err}");
        assert_eq!(seen().as_deref(), Some("100\n"));
        // A day ahead is within a wrong clock's reach, so it is taken (and remembered).
        let ahead = unix_now() + MAX_INDEX_AHEAD - 60;
        fetch_revoking(&root, &keys, OFFICIAL_SERIES, &signing, ahead, &[]).unwrap();
        // The backup's index revoking it is older than that, so it is refused as stale, but
        // the revocation takes effect all the same.
        let err = fetch_revoking(&root, &keys, OFFICIAL_SERIES, &backup, 200, &[&signing_hex])
            .unwrap_err();
        assert!(err.contains("older than an index seen before"), "{err}");
        assert_eq!(
            revoked_keys(&root),
            [crate::signature::decode_hex(&signing_hex).unwrap()]
        );
        let err =
            fetch_revoking(&root, &keys, OFFICIAL_SERIES, &signing, ahead + 1, &[]).unwrap_err();
        assert!(err.contains("does not match the release key"), "{err}");
    }

    #[test]
    fn only_the_official_keys_revoke() {
        use crate::signature::revoked_keys;
        use crate::signature::tests::key_pair;
        let (_, signing_hex) = key_pair();
        let (other, other_hex) = key_pair();
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        // `$VELT_INSTALL_PUBLIC_KEY="S,X"`: X's index revoking S is not honoured, or the
        // official S would stay refused after the variable is unset.
        let keys = [signing_hex.as_str(), other_hex.as_str()];
        fetch_revoking(&root, &keys, &signing_hex, &other, 10, &[&signing_hex]).unwrap();
        assert!(revoked_keys(&root).is_empty());
    }

    #[test]
    fn an_earlier_key_revoking_a_later_one_is_reported() {
        use crate::signature::revoked_keys;
        use crate::signature::tests::key_pair;
        let (_, signing_hex) = key_pair();
        let (_, backup_hex) = key_pair();
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        let key = |h: &str| crate::signature::decode_hex(h).unwrap();
        let trust = |series: &str| {
            trust_from(
                vec![key(&signing_hex), key(&backup_hex)],
                series.into(),
                Some(&root),
            )
            .unwrap()
        };
        let official = trust(OFFICIAL_SERIES);
        let notes =
            apply_revocations(&root, &official, &key(&signing_hex), &[key(&backup_hex)]).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("warning:"), "{}", notes[0]);
        assert!(
            notes[0].contains("only a later key may revoke") && notes[0].contains("report"),
            "{}",
            notes[0]
        );
        assert!(revoked_keys(&root).is_empty());
        // A key revoking itself is a no-op, not a warning.
        let notes =
            apply_revocations(&root, &official, &key(&signing_hex), &[key(&signing_hex)]).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        // The backup revoking the signing key says which key did it.
        let notes =
            apply_revocations(&root, &official, &key(&backup_hex), &[key(&signing_hex)]).unwrap();
        assert!(
            notes[0].contains("is revoked") && notes[0].contains("signed with a later key"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn the_official_series_starts_from_the_time_0_2_0_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        let dir = root.dir().join(INDEX_SEEN_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        // 0.2.0 kept it under its signing key, whatever keys this launcher is built with.
        std::fs::write(dir.join(LEGACY_SERIES), "100\n").unwrap();
        let index = |generated: u64| Index {
            generated,
            launcher: None,
            revoked_keys: vec![],
            releases: vec![],
        };
        let now = 1_000;
        let err = remember_index(&root, &index(50), "u", OFFICIAL_SERIES, now).unwrap_err();
        assert!(err.contains("older than an index seen before"), "{err}");
        remember_index(&root, &index(150), "u", OFFICIAL_SERIES, now).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(OFFICIAL_SERIES))
                .unwrap()
                .trim(),
            "150"
        );
        // Another build's series is its own.
        remember_index(&root, &index(5), "u", "abcd", now).unwrap();
        // More than a day ahead of the clock is refused, and not remembered.
        let err =
            remember_index(&root, &index(now + MAX_INDEX_AHEAD + 1), "u", "abcd", now).unwrap_err();
        assert!(err.contains("written in the future"), "{err}");
        remember_index(&root, &index(now + MAX_INDEX_AHEAD), "u", "abcd", now).unwrap();
    }
}
