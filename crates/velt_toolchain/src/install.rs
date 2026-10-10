//! Downloading, verifying and unpacking release archives, and putting a directory in place
//! atomically: toolchains ([`crate::release::install_toolchain`]) and target packs
//! (`velt target add`).

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// GET `url`, following redirects (release downloads redirect to a storage host), over
/// `https://` only (or `http://` to this machine). `None` when it does not exist (HTTP 404).
pub fn download(url: &str) -> Result<Option<Vec<u8>>, String> {
    let mut url = url.to_string();
    for _ in 0..5 {
        if !velt_http::is_tls_or_loopback(&url) {
            return Err(format!("refusing to download over plain http: {url}"));
        }
        let response = velt_http::fetch("GET", &url, &[("User-Agent", "velt")], &[])
            .map_err(|e| format!("cannot download {url}: {e}"))?;
        match response.status {
            200 => return Ok(Some(response.body)),
            301 | 302 | 303 | 307 | 308 => {
                url = response
                    .header("Location")
                    .ok_or_else(|| format!("{url}: a redirect without a Location"))?
                    .to_string();
            }
            404 => return Ok(None),
            status => return Err(format!("cannot download {url}: HTTP {status}")),
        }
    }
    Err(format!("too many redirects downloading {url}"))
}

/// The hash for `name` in a `SHA256SUMS`-style file (`<hex>  <name>`, or `*<name>`).
pub fn sha256_entry(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Check `bytes` against the line for `name` in a `SHA256SUMS`-style file.
pub fn check_sha256(bytes: &[u8], sums: &str, name: &str) -> Result<(), String> {
    let expected =
        sha256_entry(sums, name).ok_or_else(|| format!("SHA256SUMS has no entry for {name}"))?;
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(format!(
            "{name} has SHA-256 {actual}, but the expected one is {expected}"
        ));
    }
    Ok(())
}

/// The SHA-256 of `bytes` in lowercase hex, as `SHA256SUMS` writes it.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Unpack `archive` (a `.tar.gz` of `<top>/...`, a `what` such as "target pack") into `dest`,
/// replacing what is there only once the new directory is complete and `problem` finds nothing
/// wrong with it.
pub fn install_dir(
    archive: &[u8],
    top: &str,
    dest: &Path,
    what: &str,
    problem: impl FnOnce(&Path) -> Option<String>,
) -> Result<(), String> {
    let parent = dest
        .parent()
        .ok_or_else(|| format!("ICE: {} has no parent", dest.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let staging = parent.join(format!(".{name}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = unpack(archive, top, &staging, what).and_then(|()| match problem(&staging) {
        Some(why) => Err(format!("the {what} cannot be used: {why}")),
        None => Ok(()),
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    swap_into_place(&staging, dest)
}

/// Move the complete directory `staging` to `dest`: an installed one is renamed aside first and
/// deleted only once the new one is in place, and put back when the move fails, so `dest` is
/// never half deleted (a file of the old one still open, as Windows forbids deleting, only
/// leaves the renamed copy behind).
pub fn swap_into_place(staging: &Path, dest: &Path) -> Result<(), String> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let old = dest.with_file_name(format!(".{name}.old.{}", std::process::id()));
    let had_old = dest.exists();
    if had_old {
        if let Err(e) = std::fs::rename(dest, &old) {
            let _ = std::fs::remove_dir_all(staging);
            return Err(format!("cannot replace {} (in use?): {e}", dest.display()));
        }
    }
    if let Err(e) = std::fs::rename(staging, dest) {
        if had_old {
            let _ = std::fs::rename(&old, dest);
        }
        let _ = std::fs::remove_dir_all(staging);
        return Err(format!("cannot install into {}: {e}", dest.display()));
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Extract the regular files under `<top>/` of a `.tar.gz` into `into`, keeping whether each is
/// executable; any other path (absolute, `..`, another top directory) or entry type (links,
/// devices) is refused.
pub fn unpack(archive: &[u8], top: &str, into: &Path, what: &str) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    let entries = tar
        .entries()
        .map_err(|e| format!("not a {what} (.tar.gz): {e}"))?;
    let bad = |path: &Path, why: &str| format!("the {what} holds {}: {why}", path.display());
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read the {what}: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("cannot read the {what}: {e}"))?
            .into_owned();
        // macOS `tar` adds AppleDouble metadata (`._<name>`) beside each file; it is not ours.
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("._"))
        {
            continue;
        }
        let mut parts = path.components();
        if parts.next() != Some(Component::Normal(top.as_ref())) {
            return Err(bad(&path, &format!("everything must be under {top}/")));
        }
        let rel: PathBuf = parts.collect();
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(bad(&path, "not a plain relative path"));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(bad(&path, &format!("only files may be in a {what}")));
        }
        let out = into.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut bytes = vec![];
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("cannot read {} from the {what}: {e}", path.display()))?;
        std::fs::write(&out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.header().mode().unwrap_or(0o644);
            if mode & 0o111 != 0 {
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("cannot make {} executable: {e}", out.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `.tar.gz` holding `files` (path, contents, mode).
    pub(crate) fn archive(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, bytes, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(*mode);
            // The name goes in as given (`set_path` refuses `..`, which a hostile archive can hold).
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            tar.append(&header, *bytes).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn installs_and_replaces_a_directory_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("d/0.1.0");
        let a = archive(&[
            ("top/bin/velt", b"exe", 0o755),
            ("top/std/x.vlt", b"", 0o644),
        ]);
        install_dir(&a, "top", &dest, "toolchain", |_| None).unwrap();
        assert!(dest.join("std/x.vlt").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| {
                std::fs::metadata(dest.join(p))
                    .unwrap()
                    .permissions()
                    .mode()
            };
            assert_eq!(mode("bin/velt") & 0o111, 0o111);
            assert_eq!(mode("std/x.vlt") & 0o111, 0);
        }
        std::fs::write(dest.join("stale"), b"").unwrap();
        install_dir(&a, "top", &dest, "toolchain", |_| None).unwrap();
        assert!(!dest.join("stale").exists());
        // A rejected one leaves the installed one alone, and nothing half-installed behind.
        let err = install_dir(&a, "top", &dest, "toolchain", |_| Some("no".into())).unwrap_err();
        assert_eq!(err, "the toolchain cannot be used: no");
        assert!(dest.join("bin/velt").is_file());
        let left: Vec<_> = std::fs::read_dir(tmp.path().join("d")).unwrap().collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn refuses_unsafe_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("t");
        let other = archive(&[("other/kit.stamp", b"", 0o644)]);
        let err = install_dir(&other, "top", &dest, "target pack", |_| None).unwrap_err();
        assert!(
            err.contains("the target pack holds") && err.contains("under top/"),
            "{err}"
        );
        let escape = archive(&[("top/../../evil", b"", 0o644)]);
        assert!(install_dir(&escape, "top", &dest, "toolchain", |_| None).is_err());
        assert!(!tmp.path().join("evil").exists());
        let err = install_dir(b"not gzip", "top", &dest, "toolchain", |_| None).unwrap_err();
        assert!(err.contains("the toolchain"), "{err}");
        // macOS metadata entries are skipped, not refused.
        let meta = archive(&[("top/a", b"", 0o644), ("._top", b"meta", 0o644)]);
        install_dir(&meta, "top", &dest, "toolchain", |_| None).unwrap();
        assert!(dest.join("a").is_file());
    }

    #[test]
    fn sha256_sums() {
        let data = b"pack";
        let hash = sha256_hex(data);
        let sums = format!("0000  other.tar.gz\n{hash}  velt-1-target-x.tar.gz\n");
        check_sha256(data, &sums, "velt-1-target-x.tar.gz").unwrap();
        assert!(check_sha256(b"tampered", &sums, "velt-1-target-x.tar.gz")
            .unwrap_err()
            .contains("expected"));
        assert!(check_sha256(data, &sums, "missing.tar.gz")
            .unwrap_err()
            .contains("no entry"));
        // `sha256sum -b` writes `*<name>`.
        check_sha256(
            data,
            &format!("{hash} *velt-1-target-x.tar.gz\n"),
            "velt-1-target-x.tar.gz",
        )
        .unwrap();
    }

    #[test]
    fn a_replaced_directory_is_swapped_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let (staging, dest) = (tmp.path().join(".t.1"), tmp.path().join("t"));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("old"), b"").unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("new"), b"").unwrap();
        swap_into_place(&staging, &dest).unwrap();
        assert!(dest.join("new").exists() && !dest.join("old").exists());
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "{left:?}");
        // A failed move puts the old one back.
        let missing = tmp.path().join(".gone");
        assert!(swap_into_place(&missing, &dest).is_err());
        assert!(dest.join("new").exists());
    }
}
