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
//! - `releases.json`, every published version: `{"versions": ["0.1.0", "0.1.1"]}`. The newest
//!   release's copy, at `<base>/releases/latest/download/releases.json`, is the index the
//!   launcher reads (a list of names: what is installed is checked by the signature).

use std::path::PathBuf;

use semver::Version;

use crate::install::{check_sha256, download, install_dir, sha256_entry};
use crate::layout::{velt_exe, Root};
use crate::signature::{public_key, signed_sums};

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
        assert!(
            err.contains("has no SHA256SUMS.sig: the release is not signed"),
            "{err}"
        );
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
