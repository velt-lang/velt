//! Published releases: where they are, which versions exist, and installing one into a root.
//!
//! A release `v<version>` at `<base>/releases/download/v<version>/` holds:
//!
//! - the toolchain archive `velt-<version>-<triple>.tar.gz`, one top directory
//!   `velt-<version>-<triple>/` holding the prefix. Windows too: the release workflow publishes a
//!   `.tar.gz` beside the `.zip` the Windows installer uses (#948 part 3), so this crate unpacks
//!   one format; v0.1.0 has only the `.zip`, and installs with the installer.
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
//!   "releases": [ { "version": "0.1.0" },
//!                 { "version": "0.1.1", "yanked": "miscompiles closures; use 0.1.2" } ] }
//! ```
//!
//! Fields this launcher doesn't know are ignored, so later ones (advisories, dates) can be added
//! without breaking it; a higher `format` is a change it must not misread, and is refused.

use std::path::PathBuf;

use semver::Version;

use crate::install::{check_sha256, download, install_dir, sha256_entry};
use crate::layout::{velt_exe, Root};
use crate::signature::{public_key, signed_sums, signed_text};

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

/// A version in the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub version: Version,
    /// Why it was withdrawn: a requirement no longer selects it (an exact one still does).
    pub yanked: Option<String>,
}

/// `<base>/releases/download/index/releases.json`.
pub fn index_url(base: &str) -> String {
    format!("{base}/releases/download/{INDEX_TAG}/{INDEX_FILE}")
}

/// The releases in a `releases.json`, oldest first.
pub fn parse_index(text: &str) -> Result<Vec<Published>, String> {
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
            Ok(Published { version, yanked })
        })
        .collect::<Result<Vec<_>, String>>()?;
    releases.sort_by(|a, b| a.version.cmp(&b.version));
    releases.dedup_by(|a, b| a.version == b.version);
    Ok(releases)
}

/// The published releases, from the signed index ([`index_url`]), checked with the release key.
pub fn fetch_index(base: &str) -> Result<Vec<Published>, String> {
    fetch_index_with_key(base, None)
}

/// [`fetch_index`] checking with `key` instead of [`public_key`].
pub fn fetch_index_with_key(base: &str, key: Option<&[u8]>) -> Result<Vec<Published>, String> {
    let url = index_url(base);
    if !velt_http::is_tls_or_loopback(&url) {
        return Err(format!("refusing to download over plain http: {url}"));
    }
    let key = match key {
        Some(key) => key.to_vec(),
        None => public_key()?,
    };
    let text =
        signed_text(&url, &key)?.ok_or_else(|| format!("{url} does not exist (HTTP 404)"))?;
    parse_index(&text)
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

/// [`install_toolchain`] checking with `key` instead of [`crate::signature::public_key`].
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
    if !velt_http::is_tls_or_loopback(&release) {
        return Err(format!("refusing to download over plain http: {release}"));
    }
    let key = match key {
        Some(key) => key.to_vec(),
        None => public_key()?,
    };
    let sums = signed_sums(&release, &key)?.ok_or_else(|| missing(SUMS_FILE))?;
    if sha256_entry(&sums, &name).is_none() {
        return Err(format!(
            "velt {version} has no toolchain for {triple} (its {SUMS_FILE} lists no {name})"
        ));
    }
    let archive = download(&format!("{release}/{name}"))?.ok_or_else(|| missing(&name))?;
    check_sha256(&archive, &sums, &name)?;
    let dest = root.version_dir(version);
    install_dir(&archive, &stem, &dest, "toolchain archive", |dir| {
        let exe = velt_exe(dir);
        (!exe.is_file()).then(|| {
            format!(
                "it has no {}",
                exe.strip_prefix(dir).unwrap_or(&exe).display()
            )
        })
    })?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index() {
        let releases = parse_index(
            r#"{"format": 1, "later": true, "releases": [
                {"version": "0.2.0"}, {"version": "0.1.0"}, {"version": "0.1.0"},
                {"version": "0.2.0-rc.1", "date": "2026-10-01"},
                {"version": "0.1.1", "yanked": "miscompiles closures; use 0.1.2"},
                {"version": "0.1.2", "yanked": null}
            ]}"#,
        )
        .unwrap();
        let shown: Vec<String> = releases.iter().map(|r| r.version.to_string()).collect();
        assert_eq!(shown, ["0.1.0", "0.1.1", "0.1.2", "0.2.0-rc.1", "0.2.0"]);
        assert_eq!(
            releases[1].yanked.as_deref(),
            Some("miscompiles closures; use 0.1.2")
        );
        for bad in [
            "[]",
            "{}",
            r#"{"format": 1}"#,
            r#"{"format": 1, "releases": [1]}"#,
            r#"{"format": 1, "releases": [{"version": "x"}]}"#,
            r#"{"format": 1, "releases": [{"version": "0.1.0", "yanked": true}]}"#,
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
            r#"{"format": 1, "releases": [{"version": "0.1.0"},
                {"version": "0.1.1", "yanked": "broken"}]}"#,
        )
        .unwrap();
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
    fn serve(files: Vec<(String, Vec<u8>)>) -> velt_http::Server {
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

        // The index: signed, on the `index` release.
        let index = br#"{"format": 1, "releases": [{"version": "0.3.0"}]}"#.to_vec();
        let path = "/releases/download/index/releases.json".to_string();
        let good = serve(vec![
            (path.clone(), index.clone()),
            (format!("{path}.sig"), signer.sign(&index).as_ref().to_vec()),
        ]);
        let releases =
            fetch_index_with_key(&format!("http://{}", good.addr()), Some(&key)).unwrap();
        assert_eq!(releases[0].version, v);
        good.stop();
        let unsigned = serve(vec![(path.clone(), index.clone())]);
        let err =
            fetch_index_with_key(&format!("http://{}", unsigned.addr()), Some(&key)).unwrap_err();
        assert!(err.contains("releases.json has no signature"), "{err}");
        unsigned.stop();
        let forged = serve(vec![
            (path.clone(), index.clone()),
            (format!("{path}.sig"), forger.sign(&index).as_ref().to_vec()),
        ]);
        let err =
            fetch_index_with_key(&format!("http://{}", forged.addr()), Some(&key)).unwrap_err();
        assert!(err.contains("does not match the release key"), "{err}");
        forged.stop();
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
            err.contains("refusing to download over plain http"),
            "{err}"
        );
    }
}
