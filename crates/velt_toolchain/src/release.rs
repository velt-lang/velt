//! Published releases: where they are, which versions exist, and installing one into a root.
//!
//! A release `v<version>` at `<base>/releases/download/v<version>/` holds the toolchain archive
//! `velt-<version>-<triple>.tar.gz` (one top directory, `velt-<version>-<triple>/`, holding the
//! prefix), its `SHA256SUMS`, and `releases.json`, every published version:
//! `{"versions": ["0.1.0", "0.1.1"]}`. The newest release's copy, at
//! `<base>/releases/latest/download/releases.json`, is the index the launcher reads.

use std::path::PathBuf;

use semver::Version;

use crate::install::{check_sha256, download, install_dir, sha256_entry};
use crate::layout::{velt_exe, Root};

/// Where releases are downloaded from unless `$VELT_INSTALL_BASE_URL` says otherwise (the
/// installers read the same variable).
pub const DEFAULT_BASE_URL: &str = "https://github.com/velt-lang/velt";
/// The list of published versions in each release.
pub const INDEX_FILE: &str = "releases.json";
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

/// The versions in a `releases.json`, oldest first.
pub fn parse_index(text: &str) -> Result<Vec<Version>, String> {
    let bad = |why: String| format!("{INDEX_FILE} is not a list of releases: {why}");
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
    let list = value
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| bad("no `versions` array".into()))?;
    let mut versions = list
        .iter()
        .map(|v| {
            let s = v
                .as_str()
                .ok_or_else(|| bad(format!("{v} is not a string")))?;
            Version::parse(s).map_err(|e| bad(format!("`{s}`: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    versions.sort();
    versions.dedup();
    Ok(versions)
}

/// The published versions, from the newest release's `releases.json`.
pub fn fetch_index(base: &str) -> Result<Vec<Version>, String> {
    let url = format!("{base}/releases/latest/download/{INDEX_FILE}");
    let bytes = download(&url)?.ok_or_else(|| format!("{url} does not exist (HTTP 404)"))?;
    parse_index(&String::from_utf8_lossy(&bytes))
}

/// Download release `version` for this machine from `base`, check it against the release's
/// `SHA256SUMS`, and install it as `<root>/toolchains/<version>` (replacing one there only
/// once the new one is complete). Returns the prefix.
pub fn install_toolchain(root: &Root, version: &Version, base: &str) -> Result<PathBuf, String> {
    let triple = host_triple();
    let stem = archive_stem(version, &triple);
    let name = format!("{stem}.tar.gz");
    let release = release_url(base, version);
    let missing = |what: &str| {
        format!("velt {version} has no {what} at {release} (is {version} a published release?)")
    };
    // The expected hash first, so an archive the release lacks fails before a long download.
    let sums = download(&format!("{release}/{SUMS_FILE}"))?.ok_or_else(|| missing(SUMS_FILE))?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
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
        let versions =
            parse_index(r#"{"versions": ["0.2.0", "0.1.0", "0.1.0", "0.2.0-rc.1"]}"#).unwrap();
        let shown: Vec<String> = versions.iter().map(Version::to_string).collect();
        assert_eq!(shown, ["0.1.0", "0.2.0-rc.1", "0.2.0"]);
        for bad in [
            "[]",
            "{}",
            r#"{"versions": [1]}"#,
            r#"{"versions": ["x"]}"#,
            "nope",
        ] {
            let err = parse_index(bad).unwrap_err();
            assert!(err.starts_with("releases.json is not"), "{bad}: {err}");
        }
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
