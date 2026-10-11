//! Published releases: where they are, which versions exist, and installing one into a root.
//!
//! A release `v<version>` at `<base>/releases/download/v<version>/` holds:
//!
//! - the toolchain archive `velt-<version>-<triple>.tar.gz`, one top directory
//!   `velt-<version>-<triple>/` holding the prefix. Windows too: the release workflow publishes a
//!   `.tar.gz` beside the `.zip` the Windows installer uses, so this crate unpacks one format
//!   (v0.1.0 has only the `.zip`).
//! - `SHA256SUMS`, and `SHA256SUMS.sig`, its signature with the release key
//!   ([`crate::signature`]): the hash shows an archive is intact, the signature that the velt
//!   project published it. Releases before signing (v0.1.0) cannot be installed from here.
//!
//! The index of releases is one file that the release workflow rewrites on every release
//! (stable or pre-release) and whenever a version is yanked: `releases.json` and its signature
//! `releases.json.sig`, on a permanent release tagged `index` (a pre-release, so it is never
//! GitHub's "latest"): `<base>/releases/download/index/releases.json`. A mirror copies it like
//! any other release file. The format ([`parse_index`]):
//!
//! ```json
//! { "format": 1,
//!   "generated": 1760100000,
//!   "launcher": "0.2.0",
//!   "releases": [ { "version": "0.1.0" },
//!                 { "version": "0.1.1", "yanked": "miscompiles closures; use 0.1.2" } ] }
//! ```
//!
//! - `generated`: when the workflow wrote it (Unix seconds). The newest one seen is kept in
//!   `<root>/index-seen/official` (another build's releases, `$VELT_INSTALL_PUBLIC_KEY`: under
//!   their first key, hex), and an older index of those releases is refused: its signature is
//!   valid, but a mirror serving it (behind, or on purpose) would hide later releases and
//!   yanks. One series for the official releases whichever key signs, so it survives a
//!   rotation; it starts from what 0.2.0 kept under its signing key. An index written more than
//!   a day ahead of this machine's clock is refused, and never remembered.
//! - `revoked_keys` (optional): release keys withdrawn because they leaked, in hex. A launcher
//!   honours one only when the index is signed by an official key after it in its trust list
//!   (the backup revokes the signing key, never the other way round, or a leaked signing key
//!   could revoke the backup first; an index that tries is reported), and keeps it revoked for
//!   good (`<root>/revoked-keys`): a later index that leaves it out doesn't undo it.
//!   Revocations are applied before the freshness check, so an index dated ahead by the leaked
//!   key can't block the one revoking it.
//! - `launcher`: the newest launcher's version (optional); an older launcher says to run the
//!   installer again, so launchers already out there learn about newer ones.
//!
//! Fields this launcher doesn't know are ignored, so later ones (advisories, dates) can be added
//! without breaking it; a higher `format` is a change it must not misread, and is refused.

mod index_trust;

use std::io::Read;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::install::{
    check_download_url, check_sha256, download, install_dir, sha256_entry, Existing, MAX_ARCHIVE,
};
use crate::layout::{velt_exe, Root};
use crate::signature::{
    hex_of, record_revoked, signed_sums, signed_text_by, trust, trust_from, verify_by, Trust,
    LEGACY_SERIES, OFFICIAL_SERIES,
};
pub use index_trust::MAX_INDEX_AHEAD;
use index_trust::{apply_revocations, remember_index, unix_now};

/// Where releases are downloaded from unless `$VELT_INSTALL_BASE_URL` says otherwise (the
/// installers read the same variable).
pub const DEFAULT_BASE_URL: &str = "https://github.com/velt-lang/velt";
/// The index of releases, on the [`INDEX_TAG`] release.
pub const INDEX_FILE: &str = "releases.json";
/// The release that holds the index.
pub const INDEX_TAG: &str = "index";
/// The index format this launcher reads.
pub const INDEX_FORMAT: u64 = 1;
pub const SUMS_FILE: &str = "SHA256SUMS";

/// `$VELT_INSTALL_BASE_URL` (a mirror, or a test's server), else [`DEFAULT_BASE_URL`].
pub fn base_url() -> String {
    std::env::var("VELT_INSTALL_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.into())
        .trim_end_matches('/')
        .to_string()
}

/// The directory of release `version`'s assets.
pub fn release_url(base: &str, version: &Version) -> String {
    format!("{base}/releases/download/v{version}")
}

/// The machine this program runs on, as release archives name it (`velt_link::host_triple`,
/// which this crate does not depend on).
pub fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        _ if cfg!(target_env = "musl") => format!("{arch}-unknown-linux-musl"),
        _ => format!("{arch}-unknown-linux-gnu"),
    }
}

/// `velt-<version>-<triple>`: the toolchain archive's top directory, and its name without
/// `.tar.gz`.
pub fn archive_stem(version: &Version, triple: &str) -> String {
    format!("velt-{version}-{triple}")
}

/// The index of releases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    /// When it was written (Unix seconds).
    pub generated: u64,
    /// The newest launcher's version.
    pub launcher: Option<Version>,
    /// Release keys withdrawn because they leaked (`revoked_keys`, hex): a launcher honours one
    /// only when the index is signed by a key after it in its trust list (the backup revokes
    /// the signing key, never the other way round), and keeps it revoked for good
    /// ([`crate::signature::REVOKED_KEYS_FILE`]).
    pub revoked_keys: Vec<Vec<u8>>,
    /// Oldest first.
    pub releases: Vec<Published>,
}

impl Index {
    /// The newest launcher, when it is newer than `own`.
    pub fn newer_launcher(&self, own: &Version) -> Option<&Version> {
        self.launcher.as_ref().filter(|l| *l > own)
    }
}

/// The directory in the root that remembers the newest index seen, one file per release key.
pub const INDEX_SEEN_DIR: &str = "index-seen";

/// A version in the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub version: Version,
    /// Why it was withdrawn: a requirement no longer selects it (an exact one still does).
    pub yanked: Option<String>,
    /// A problem in this version (`advisory`), which the launcher tells users of it about.
    pub advisory: Option<String>,
    /// Whether the problem is a security one (`security: true`).
    pub security: bool,
}

/// `<base>/releases/download/index/releases.json`.
pub fn index_url(base: &str) -> String {
    format!("{base}/releases/download/{INDEX_TAG}/{INDEX_FILE}")
}

/// A `releases.json`.
pub fn parse_index(text: &str) -> Result<Index, String> {
    let bad = |why: String| format!("{INDEX_FILE} is not an index of releases: {why}");
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
    match value.get("format").and_then(|f| f.as_u64()) {
        Some(INDEX_FORMAT) => {}
        Some(n) => {
            return Err(format!(
            "{INDEX_FILE} has format {n}, newer than this velt launcher reads ({INDEX_FORMAT}); \
                 update the launcher (run the installer again)"
        ))
        }
        None => return Err(bad("no `format` number".into())),
    }
    let generated = value
        .get("generated")
        .and_then(|g| g.as_u64())
        .ok_or_else(|| bad("no `generated` time".into()))?;
    let launcher = match value.get("launcher") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(v)) => {
            Some(Version::parse(v).map_err(|e| bad(format!("`launcher` `{v}`: {e}")))?)
        }
        Some(other) => return Err(bad(format!("`launcher` is {other}, not a version"))),
    };
    let revoked_keys = match value.get("revoked_keys") {
        None | Some(serde_json::Value::Null) => vec![],
        Some(serde_json::Value::Array(keys)) => keys
            .iter()
            .map(|k| {
                k.as_str()
                    .and_then(crate::signature::decode_hex)
                    .filter(|k| k.len() == 32)
                    .ok_or_else(|| {
                        bad(format!(
                            "`revoked_keys` holds {k}, not a key (64 hex digits)"
                        ))
                    })
            })
            .collect::<Result<_, _>>()?,
        Some(other) => return Err(bad(format!("`revoked_keys` is {other}, not an array"))),
    };
    let list = value
        .get("releases")
        .and_then(|v| v.as_array())
        .ok_or_else(|| bad("no `releases` array".into()))?;
    let mut releases = list
        .iter()
        .map(|entry| {
            let version = entry
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| bad(format!("{entry} has no `version` string")))?;
            let version = Version::parse(version).map_err(|e| bad(format!("`{version}`: {e}")))?;
            let yanked = match entry.get("yanked") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(why)) => Some(why.clone()),
                Some(other) => {
                    return Err(bad(format!(
                        "`yanked` of {version} is {other}, not a string"
                    )))
                }
            };
            let advisory = match entry.get("advisory") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                Some(other) => {
                    return Err(bad(format!(
                        "`advisory` of {version} is {other}, not a string"
                    )))
                }
            };
            let security = match entry.get("security") {
                None | Some(serde_json::Value::Null) => false,
                Some(serde_json::Value::Bool(b)) => *b,
                Some(other) => {
                    return Err(bad(format!(
                        "`security` of {version} is {other}, not a boolean"
                    )))
                }
            };
            Ok(Published {
                version,
                yanked,
                advisory,
                security,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    releases.sort_by(|a, b| a.version.cmp(&b.version));
    releases.dedup_by(|a, b| a.version == b.version);
    Ok(Index {
        generated,
        launcher,
        revoked_keys,
        releases,
    })
}

/// The signed index of releases ([`index_url`]), checked with the release key and against the
/// newest index `root` has seen, which it then remembers.
pub fn fetch_index(root: &Root, base: &str) -> Result<Index, String> {
    fetch_index_with_key(root, base, None)
}

/// [`fetch_index`] checking with `key` instead of [`crate::signature::public_keys`].
pub fn fetch_index_with_key(root: &Root, base: &str, key: Option<&[u8]>) -> Result<Index, String> {
    let url = index_url(base);
    check_download_url(&url)?;
    fetch_index_trusting(root, &url, &trusted(root, key)?)
}

/// [`fetch_index`] from `url` with `trust`.
fn fetch_index_trusting(root: &Root, url: &str, trust: &Trust) -> Result<Index, String> {
    let (text, signer) = signed_text_by(url, &trust.keys)?
        .ok_or_else(|| format!("{url} does not exist (HTTP 404)"))?;
    let index = parse_index(&text)?;
    // Revocations first, from any validly signed index: a stale or replayed one can't harm
    // (they only grow, and only a later key revokes), and an index a leaked key dated far
    // ahead can't block the one revoking it.
    for note in apply_revocations(root, trust, &trust.keys[signer], &index.revoked_keys)? {
        eprintln!("velt: {note}");
    }
    remember_index(root, &index, url, &trust.series, unix_now())?;
    // The newest verified index, for notices about the version that runs (no network then).
    let _ = crate::layout::write_file(&root.dir().join(INDEX_CACHE_FILE), &text);
    Ok(index)
}

/// The copy of the newest index fetched ([`fetch_index`]), verified then: what notices about a
/// version use without the network.
pub const INDEX_CACHE_FILE: &str = "index-cache.json";

/// The index [`fetch_index`] last fetched into `root`, if any.
pub fn cached_index(root: &Root) -> Option<Index> {
    let text = std::fs::read_to_string(root.dir().join(INDEX_CACHE_FILE)).ok()?;
    parse_index(&text).ok()
}

/// What users of `version` should know, from `index`: it is yanked, or has an advisory; with
/// the newest release of its minor that is neither, when there is one. The index's text goes
/// through [`display_text`].
pub fn notice_for(index: &Index, version: &Version) -> Option<String> {
    let entry = index.releases.iter().find(|r| &r.version == version)?;
    let yanked = entry.yanked.as_deref().map(display_text);
    let advisory = entry.advisory.as_deref().map(display_text);
    if yanked.is_none() && advisory.is_none() {
        return None;
    }
    let mut problem = format!("velt {version}");
    if entry.security {
        problem.push_str(" has a security problem");
    }
    match (yanked, advisory) {
        (Some(why), advisory) => {
            let and = if entry.security { " and" } else { "" };
            problem.push_str(&format!("{and} is yanked: {why}"));
            if let Some(text) = advisory {
                problem.push_str(&format!("; {text}"));
            }
        }
        (None, Some(text)) => problem.push_str(&format!(": {text}")),
        (None, None) => {}
    }
    let fixed = index
        .releases
        .iter()
        .filter(|r| {
            r.version.major == version.major
                && r.version.minor == version.minor
                && r.version > *version
                && r.version.pre.is_empty()
                && r.yanked.is_none()
                && r.advisory.is_none()
        })
        .map(|r| &r.version)
        .max();
    Some(match fixed {
        Some(v) => format!(
            "{problem}; velt {v} doesn't have it (`velt update` in a package pinned to it, or \
             `velt toolchain install {v}`)"
        ),
        None => problem,
    })
}

/// The longest [`display_text`] keeps, in characters.
pub const DISPLAY_TEXT_MAX: usize = 200;

/// Text from the index of releases (a yank reason, an advisory) made safe to print: signed, but
/// still not trusted with the terminal. Control characters (escape sequences) and bidirectional
/// overrides become `?`, line breaks and tabs a space, and it is cut at [`DISPLAY_TEXT_MAX`]
/// characters with `…`.
pub fn display_text(text: &str) -> String {
    let mut shown = String::new();
    for (i, c) in text.trim().chars().enumerate() {
        if i == DISPLAY_TEXT_MAX {
            shown.push('…');
            break;
        }
        shown.push(match c {
            '\n' | '\r' | '\t' => ' ',
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '?',
            c if c.is_control() => '?',
            c => c,
        });
    }
    shown
}

/// What `root` trusts: `key` (a test's), else the built-in keys
/// ([`crate::signature::public_keys`]), without the revoked ones.
fn trusted(root: &Root, key: Option<&[u8]>) -> Result<Trust, String> {
    match key {
        Some(key) => trust_from(vec![key.to_vec()], hex_of(key), Some(root)),
        None => trust(Some(root)),
    }
}

/// The newest release `req` accepts that is not yanked (an exact requirement may name a yanked
/// one: the user chose it).
pub fn newest_match<'r>(
    releases: &'r [Published],
    req: &crate::Requirement,
) -> Option<&'r Published> {
    let exact = req.exact_version();
    releases
        .iter()
        .filter(|r| req.matches(&r.version))
        .filter(|r| r.yanked.is_none() || exact.as_ref() == Some(&r.version))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Download release `version` for this machine from `base`, check it against the release's
/// signed `SHA256SUMS` (authentic, not only intact: [`crate::signature`]), and install it as
/// `<root>/toolchains/<version>` (replacing one there only once the new one is complete).
/// Returns the prefix.
pub fn install_toolchain(root: &Root, version: &Version, base: &str) -> Result<PathBuf, String> {
    install_toolchain_with_key(root, version, base, None)
}

/// [`install_toolchain`] checking with `key` instead of [`crate::signature::public_keys`].
pub fn install_toolchain_with_key(
    root: &Root,
    version: &Version,
    base: &str,
    key: Option<&[u8]>,
) -> Result<PathBuf, String> {
    let triple = host_triple();
    let stem = archive_stem(version, &triple);
    let name = format!("{stem}.tar.gz");
    let release = release_url(base, version);
    let missing = |what: &str| {
        format!("velt {version} has no {what} at {release} (is {version} a published release?)")
    };
    // The expected hash first, so an archive the release lacks fails before a long download.
    check_download_url(&release)?;
    let trust = trusted(root, key)?;
    let sums = signed_sums(&release, &trust.keys)?.ok_or_else(|| missing(SUMS_FILE))?;
    if sha256_entry(&sums, &name).is_none() {
        return Err(format!(
            "velt {version} has no toolchain for {triple} (its {SUMS_FILE} lists no {name})"
        ));
    }
    let archive =
        download(&format!("{release}/{name}"), MAX_ARCHIVE)?.ok_or_else(|| missing(&name))?;
    check_sha256(&archive, &sums, &name)?;
    install_archive(root, version, &stem, &archive)
}

/// Unpack the checked toolchain `archive` (top directory `stem`) as `<root>/toolchains/<version>`.
fn install_archive(
    root: &Root,
    version: &Version,
    stem: &str,
    archive: &[u8],
) -> Result<PathBuf, String> {
    let dest = root.version_dir(version);
    // A version never changes: one installed meanwhile (another launcher) is kept, not
    // swapped out from under the commands running it.
    install_dir(
        archive,
        stem,
        &dest,
        "toolchain archive",
        Existing::Keep,
        |dir| {
            let exe = velt_exe(dir);
            (!exe.is_file()).then(|| {
                format!(
                    "it has no {}",
                    exe.strip_prefix(dir).unwrap_or(&exe).display()
                )
            })
        },
    )?;
    Ok(dest)
}

/// Install a toolchain archive downloaded before (`velt toolchain install --from`, for machines
/// without access to the releases): `velt-<version>-<this machine>.tar.gz`, checked against the
/// `SHA256SUMS` and `SHA256SUMS.sig` of its release beside it, with the keys `root` trusts (`key`
/// for a test), unless `unverified`. Returns the version, its prefix, and whether it was
/// installed now (`false`: that version was installed already, and is kept as it is).
pub fn install_toolchain_from_file(
    root: &Root,
    file: &Path,
    unverified: bool,
    key: Option<&[u8]>,
) -> Result<(Version, PathBuf, bool), String> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let host = host_triple();
    let version = name
        .strip_prefix("velt-")
        .and_then(|rest| rest.strip_suffix(&format!("-{host}.tar.gz")))
        .and_then(|v| Version::parse(v).ok())
        .ok_or_else(|| {
            format!(
                "{} is not a toolchain archive for this machine: its name must be \
                 velt-<version>-{host}.tar.gz",
                file.display()
            )
        })?;
    if root.versions().contains(&version) {
        // A version never changes: the one installed is kept, and the file isn't read.
        let prefix = root.version_dir(&version);
        return Ok((version, prefix, false));
    }
    let mut archive = vec![];
    // Read through a limit, not after checking the size: the file may grow meanwhile.
    std::fs::File::open(file)
        .and_then(|f| f.take(MAX_ARCHIVE as u64 + 1).read_to_end(&mut archive))
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    if archive.len() > MAX_ARCHIVE {
        return Err(format!(
            "{} is larger than a toolchain archive",
            file.display()
        ));
    }
    if unverified {
        eprintln!(
            "velt: warning: installing {} without checking it (--unverified)",
            file.display()
        );
    } else {
        let beside = |n: &str| file.with_file_name(n);
        let read = |p: &Path| std::fs::read(p).ok();
        let (Some(sums), Some(signature)) = (
            read(&beside(SUMS_FILE)),
            read(&beside(&format!("{SUMS_FILE}.sig"))),
        ) else {
            return Err(format!(
                "cannot check {}: put its release's {SUMS_FILE} and {SUMS_FILE}.sig beside it, or \
                 pass --unverified to install it unchecked",
                file.display()
            ));
        };
        let trust = trusted(root, key)?;
        verify_by(&sums, &signature, &trust.keys)
            .map_err(|e| format!("{} beside {}: {e}; refusing it", SUMS_FILE, file.display()))?;
        let sums = String::from_utf8(sums)
            .map_err(|_| format!("{SUMS_FILE} beside {} is not text", file.display()))?;
        check_sha256(&archive, &sums, &name)?;
    }
    let stem = archive_stem(&version, &host);
    let prefix = install_archive(root, &version, &stem, &archive)?;
    Ok((version, prefix, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index() {
        let index = parse_index(
            r#"{"format": 1, "generated": 5, "launcher": "0.2.0", "later": true, "releases": [
                {"version": "0.2.0"}, {"version": "0.1.0"}, {"version": "0.1.0"},
                {"version": "0.2.0-rc.1", "date": "2026-10-01"},
                {"version": "0.1.1", "yanked": "miscompiles closures; use 0.1.2"},
                {"version": "0.1.2", "yanked": null}
            ]}"#,
        )
        .unwrap();
        assert_eq!(index.generated, 5);
        let v = |s: &str| Version::parse(s).unwrap();
        assert_eq!(index.newer_launcher(&v("0.1.9")), Some(&v("0.2.0")));
        assert_eq!(index.newer_launcher(&v("0.2.0")), None);
        let releases = index.releases;
        let shown: Vec<String> = releases.iter().map(|r| r.version.to_string()).collect();
        assert_eq!(shown, ["0.1.0", "0.1.1", "0.1.2", "0.2.0-rc.1", "0.2.0"]);
        assert_eq!(
            releases[1].yanked.as_deref(),
            Some("miscompiles closures; use 0.1.2")
        );
        for bad in [
            "[]",
            "{}",
            r#"{"format": 1, "generated": 1}"#,
            r#"{"format": 1, "releases": []}"#,
            r#"{"format": 1, "generated": "now", "releases": []}"#,
            r#"{"format": 1, "generated": 1, "launcher": 2, "releases": []}"#,
            r#"{"format": 1, "generated": 1, "releases": [1]}"#,
            r#"{"format": 1, "generated": 1, "releases": [{"version": "x"}]}"#,
            r#"{"format": 1, "generated": 1, "releases": [{"version": "0.1.0", "yanked": true}]}"#,
            r#"{"versions": ["0.1.0"]}"#,
            "nope",
        ] {
            let err = parse_index(bad).unwrap_err();
            assert!(
                err.starts_with("releases.json is not an index"),
                "{bad}: {err}"
            );
        }
        let err = parse_index(r#"{"format": 2, "releases": []}"#).unwrap_err();
        assert!(err.contains("newer than this velt launcher reads"), "{err}");
    }

    #[test]
    fn yanked_releases_are_selected_only_by_name() {
        let releases = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [{"version": "0.1.0"},
                {"version": "0.1.1", "yanked": "broken"}]}"#,
        )
        .unwrap()
        .releases;
        let newest = |req: &str| {
            newest_match(&releases, &crate::Requirement::parse(req).unwrap())
                .map(|r| r.version.to_string())
        };
        assert_eq!(newest("0.1").as_deref(), Some("0.1.0"));
        assert_eq!(newest("=0.1.1").as_deref(), Some("0.1.1"));
        assert_eq!(newest("0.1.1"), None);
        assert_eq!(newest("0.2"), None);
    }

    #[test]
    fn names() {
        let v = Version::parse("0.1.0").unwrap();
        assert_eq!(
            release_url("https://x.example", &v),
            "https://x.example/releases/download/v0.1.0"
        );
        assert_eq!(
            archive_stem(&v, "x86_64-apple-darwin"),
            "velt-0.1.0-x86_64-apple-darwin"
        );
        assert!(host_triple().contains(std::env::consts::ARCH));
    }

    /// Serve `files` (path → bytes) on loopback for the life of the returned server.
    pub(super) fn serve(files: Vec<(String, Vec<u8>)>) -> velt_http::Server {
        let files: std::collections::HashMap<String, Vec<u8>> = files.into_iter().collect();
        let handler =
            std::sync::Arc::new(move |req: velt_http::Request| match files.get(&req.path) {
                Some(b) => velt_http::Response::bytes(200, "application/octet-stream", b.clone()),
                None => velt_http::Response::text(404, "not found"),
            });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        velt_http::Server::start(listener, handler, 1 << 20).unwrap()
    }

    #[test]
    fn installs_a_signed_release_and_refuses_an_unsigned_or_forged_one() {
        use crate::install::tests::archive;
        use crate::signature::tests::key_pair;
        let (signer, public) = key_pair();
        let key: Vec<u8> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&public[i..i + 2], 16).unwrap())
            .collect();
        let v = Version::parse("0.3.0").unwrap();
        let stem = archive_stem(&v, &host_triple());
        let exe = format!("{stem}/bin/velt{}", std::env::consts::EXE_SUFFIX);
        let tgz = archive(&[(&exe, b"exe", 0o755)]);
        let sums = format!("{}  {stem}.tar.gz\n", crate::install::sha256_hex(&tgz));
        let dir = "/releases/download/v0.3.0";
        let release = |sig: Option<Vec<u8>>| {
            let mut files = vec![
                (format!("{dir}/{stem}.tar.gz"), tgz.clone()),
                (format!("{dir}/SHA256SUMS"), sums.clone().into_bytes()),
            ];
            if let Some(sig) = sig {
                files.push((format!("{dir}/SHA256SUMS.sig"), sig));
            }
            serve(files)
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        let install = |server: &velt_http::Server| {
            let base = format!("http://{}", server.addr());
            install_toolchain_with_key(&root, &v, &base, Some(&key))
        };

        let unsigned = release(None);
        let err = install(&unsigned).unwrap_err();
        assert!(err.contains("SHA256SUMS has no signature"), "{err}");
        unsigned.stop();
        let (forger, _) = key_pair();
        let forged = release(Some(forger.sign(sums.as_bytes()).as_ref().to_vec()));
        let err = install(&forged).unwrap_err();
        assert!(err.contains("does not match the release key"), "{err}");
        forged.stop();
        assert!(root.versions().is_empty());

        let signed = release(Some(signer.sign(sums.as_bytes()).as_ref().to_vec()));
        let prefix = install(&signed).unwrap();
        assert_eq!(root.versions(), std::slice::from_ref(&v));
        assert!(crate::layout::velt_exe(&prefix).is_file());
        let missing = install_toolchain_with_key(
            &root,
            &Version::parse("0.4.0").unwrap(),
            &format!("http://{}", signed.addr()),
            Some(&key),
        )
        .unwrap_err();
        assert!(
            missing.contains("is 0.4.0 a published release?"),
            "{missing}"
        );
        signed.stop();

        // The index: signed, on the `index` release, never older than one seen before.
        let signed_index = |generated: u64, by: &ring::signature::Ed25519KeyPair| {
            let text = format!(
                r#"{{"format": 1, "generated": {generated}, "releases": [{{"version": "0.3.0"}}]}}"#
            );
            let sig = by.sign(text.as_bytes()).as_ref().to_vec();
            (text.into_bytes(), sig)
        };
        let path = "/releases/download/index/releases.json".to_string();
        let index_server = |index: (Vec<u8>, Vec<u8>), signed: bool| {
            let mut files = vec![(path.clone(), index.0)];
            if signed {
                files.push((format!("{path}.sig"), index.1));
            }
            serve(files)
        };
        let fetch = |server: &velt_http::Server| {
            fetch_index_with_key(&root, &format!("http://{}", server.addr()), Some(&key))
        };
        let good = index_server(signed_index(10, &signer), true);
        assert_eq!(fetch(&good).unwrap().releases[0].version, v);
        good.stop();
        let unsigned = index_server(signed_index(11, &signer), false);
        let err = fetch(&unsigned).unwrap_err();
        assert!(err.contains("releases.json has no signature"), "{err}");
        unsigned.stop();
        let forged = index_server(signed_index(12, &forger), true);
        let err = fetch(&forged).unwrap_err();
        assert!(err.contains("does not match the release key"), "{err}");
        forged.stop();
        // A forged one is not remembered: the same time again is fine, an older one is not.
        let same = index_server(signed_index(10, &signer), true);
        fetch(&same).unwrap();
        same.stop();
        let old = index_server(signed_index(9, &signer), true);
        let err = fetch(&old).unwrap_err();
        assert!(err.contains("is older than an index seen before"), "{err}");
        assert!(err.contains("index-seen"), "{err}");
        old.stop();
        let newer = index_server(signed_index(20, &signer), true);
        fetch(&newer).unwrap();
        newer.stop();
        let seen = std::fs::read_to_string(root.dir().join(INDEX_SEEN_DIR).join(&public)).unwrap();
        assert_eq!(seen.trim(), "20");
        // Another key's releases have times of their own.
        let (other_signer, other_public) = key_pair();
        let other_key: Vec<u8> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&other_public[i..i + 2], 16).unwrap())
            .collect();
        let fork = index_server(signed_index(3, &other_signer), true);
        fetch_index_with_key(&root, &format!("http://{}", fork.addr()), Some(&other_key)).unwrap();
        fork.stop();
        let official = index_server(signed_index(20, &signer), true);
        fetch(&official).unwrap();
        official.stop();
    }

    #[test]
    fn plain_http_elsewhere_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let err = install_toolchain(
            &Root::new(tmp.path()),
            &Version::parse("0.1.0").unwrap(),
            "http://releases.example",
        )
        .unwrap_err();
        assert!(
            err.contains("refusing to download http://releases.example")
                && err.contains("plain http:// to another machine"),
            "{err}"
        );
    }

    #[test]
    fn revoked_keys_in_the_index_are_keys() {
        let k = "11".repeat(32);
        let index = parse_index(&format!(
            r#"{{"format": 1, "generated": 1, "revoked_keys": ["{k}"], "releases": []}}"#
        ))
        .unwrap();
        assert_eq!(index.revoked_keys, [vec![0x11; 32]]);
        for bad in [r#"["zz"]"#, r#"["11"]"#, r#""x""#, "[1]"] {
            let text = format!(
                r#"{{"format": 1, "generated": 1, "revoked_keys": {bad}, "releases": []}}"#
            );
            let err = parse_index(&text).unwrap_err();
            assert!(err.contains("revoked_keys"), "{bad}: {err}");
        }
    }

    #[test]
    fn notices_for_yanked_and_advised_versions() {
        let index = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [
                {"version": "0.2.0", "advisory": "TLS certificates are not checked", "security": true},
                {"version": "0.2.1", "yanked": "miscompiles closures"},
                {"version": "0.2.2"},
                {"version": "0.2.3-rc.1"},
                {"version": "0.3.0", "advisory": "slow startup"}
            ]}"#,
        )
        .unwrap();
        let v = |s: &str| Version::parse(s).unwrap();
        let security = notice_for(&index, &v("0.2.0")).unwrap();
        assert!(
            security.starts_with("velt 0.2.0 has a security problem: TLS certificates")
                && security.contains("velt 0.2.2 doesn't have it"),
            "{security}"
        );
        let yanked = notice_for(&index, &v("0.2.1")).unwrap();
        assert!(
            yanked.starts_with("velt 0.2.1 is yanked: miscompiles closures"),
            "{yanked}"
        );
        assert_eq!(notice_for(&index, &v("0.2.2")), None);
        assert_eq!(
            notice_for(&index, &v("0.3.0")).unwrap(),
            "velt 0.3.0: slow startup"
        );
        assert_eq!(notice_for(&index, &v("9.9.9")), None);
        // The fix it suggests.
        assert!(
            security.ends_with(
                "(`velt update` in a package pinned to it, or `velt toolchain install 0.2.2`)"
            ),
            "{security}"
        );
        // Yanked for a security problem: both said.
        let index = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [
                {"version": "0.4.0", "yanked": "use 0.4.1", "advisory": "a crafted URL reads files",
                 "security": true}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            notice_for(&index, &v("0.4.0")).unwrap(),
            "velt 0.4.0 has a security problem and is yanked: use 0.4.1; a crafted URL reads files"
        );
        for bad in [
            r#"{"version": "0.1.0", "advisory": 1}"#,
            r#"{"version": "0.1.0", "security": "yes"}"#,
        ] {
            let text = format!(r#"{{"format": 1, "generated": 1, "releases": [{bad}]}}"#);
            assert!(parse_index(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn text_from_the_index_is_safe_to_print() {
        assert_eq!(display_text("  plain text "), "plain text");
        assert_eq!(
            display_text("\u{1b}[31mred\u{1b}]0;title\u{7}\r\nvelt: fake\tline"),
            "?[31mred?]0;title?  velt: fake line"
        );
        assert_eq!(display_text("a\u{202e}b\u{2066}c\u{7f}"), "a?b?c?");
        let long = display_text(&"x".repeat(20_000));
        assert_eq!(long.chars().count(), DISPLAY_TEXT_MAX + 1);
        assert!(long.ends_with('…'));
        assert_eq!(
            display_text(&"é".repeat(DISPLAY_TEXT_MAX)),
            "é".repeat(DISPLAY_TEXT_MAX)
        );
        // Notices use it.
        let index = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [
                {"version": "0.1.0", "advisory": "\u001b[2Jgone\nvelt: all good"}]}"#,
        )
        .unwrap();
        let notice = notice_for(&index, &Version::parse("0.1.0").unwrap()).unwrap();
        assert_eq!(notice, "velt 0.1.0: ?[2Jgone velt: all good");
    }
}
