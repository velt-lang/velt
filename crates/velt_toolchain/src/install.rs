//! Downloading, verifying and unpacking release archives, and putting a directory in place
//! atomically: toolchains ([`crate::release::install_toolchain`]) and target packs
//! (`velt target add`).

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

/// The most an archive may unpack to: a gzip bomb (a `--from` pack, an archive from a mirror
/// before its hash is checked is never unpacked, but a file given by path is) stops here.
/// Toolchains unpack to a few hundred MiB.
#[derive(Clone, Copy, Debug)]
pub struct UnpackLimits {
    /// One file.
    pub entry: u64,
    /// All files together.
    pub total: u64,
}

impl UnpackLimits {
    pub const DEFAULT: UnpackLimits = UnpackLimits {
        entry: 1 << 30,
        total: 4 << 30,
    };
}

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

/// What [`install_dir`] does when `dest` is already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    /// Swap it out for the new one (a target pack added again).
    Replace,
    /// Keep it and drop the new one: what is there is the same thing, installed by another
    /// process meanwhile (a toolchain version never changes), and may be in use.
    Keep,
}

/// Unpack `archive` (a `.tar.gz` of `<top>/...`, a `what` such as "target pack") into `dest`,
/// putting it in place only once the new directory is complete and `problem` finds nothing
/// wrong with it.
pub fn install_dir(
    archive: &[u8],
    top: &str,
    dest: &Path,
    what: &str,
    existing: Existing,
    problem: impl FnOnce(&Path) -> Option<String>,
) -> Result<(), String> {
    let parent = dest
        .parent()
        .ok_or_else(|| format!("ICE: {} has no parent", dest.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let staging = parent.join(format!(".{name}.{}", unique()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = unpack(archive, top, &staging, what).and_then(|()| match problem(&staging) {
        Some(why) => Err(format!("the {what} cannot be used: {why}")),
        None => Ok(()),
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    match existing {
        Existing::Replace => swap_into_place(&staging, dest),
        Existing::Keep => {
            // A rename onto an existing directory fails (it is never empty), so whichever
            // process renames first wins and the others keep its copy.
            let moved = if dest.exists() {
                Ok(())
            } else {
                std::fs::rename(&staging, dest)
            };
            let _ = std::fs::remove_dir_all(&staging);
            match moved {
                Err(_) if dest.exists() => Ok(()),
                Err(e) => Err(format!("cannot install into {}: {e}", dest.display())),
                Ok(()) => Ok(()),
            }
        }
    }
}

/// `<pid>-<n>`: a name no other install, in this process or another, is using.
fn unique() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

/// Move the complete directory `staging` to `dest`: an installed one is renamed aside first and
/// deleted only once the new one is in place, and put back when the move fails, so `dest` is
/// never half deleted (a file of the old one still open, as Windows forbids deleting, only
/// leaves the renamed copy behind).
pub fn swap_into_place(staging: &Path, dest: &Path) -> Result<(), String> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let old = dest.with_file_name(format!(".{name}.old.{}", unique()));
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
/// executable; any other path (absolute, `..`, another top directory, `<top>` itself) or entry
/// type (links, devices) is refused, and so is more than [`UnpackLimits::DEFAULT`].
pub fn unpack(archive: &[u8], top: &str, into: &Path, what: &str) -> Result<(), String> {
    unpack_within(archive, top, into, what, UnpackLimits::DEFAULT)
}

/// [`unpack`] with other limits.
pub fn unpack_within(
    archive: &[u8],
    top: &str,
    into: &Path,
    what: &str,
    limits: UnpackLimits,
) -> Result<(), String> {
    let mut total: u64 = 0;
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
        if rel.as_os_str().is_empty() {
            return Err(bad(&path, &format!("{top} must be a directory")));
        }
        let too_big = || {
            bad(
                &path,
                "more data than a release holds (a damaged or hostile archive)",
            )
        };
        if entry.size() > limits.entry || total.saturating_add(entry.size()) > limits.total {
            return Err(too_big());
        }
        let out = into.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut file = std::fs::File::create(&out)
            .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        // The header's size is checked above; `take` holds even if the data runs longer.
        let written = std::io::copy(&mut (&mut entry).take(limits.entry + 1), &mut file)
            .map_err(|e| format!("cannot unpack {} from the {what}: {e}", path.display()))?;
        total += written;
        if written > limits.entry || total > limits.total {
            return Err(too_big());
        }
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
        install_dir(&a, "top", &dest, "toolchain", Existing::Replace, |_| None).unwrap();
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
        install_dir(&a, "top", &dest, "toolchain", Existing::Replace, |_| None).unwrap();
        assert!(!dest.join("stale").exists());
        // A rejected one leaves the installed one alone, and nothing half-installed behind.
        let err = install_dir(&a, "top", &dest, "toolchain", Existing::Replace, |_| {
            Some("no".into())
        })
        .unwrap_err();
        assert_eq!(err, "the toolchain cannot be used: no");
        assert!(dest.join("bin/velt").is_file());
        let left: Vec<_> = std::fs::read_dir(tmp.path().join("d")).unwrap().collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    /// One tar entry of any kind, its name written as given.
    fn entry(name: &[u8], kind: tar::EntryType, link: &[u8], data: &[u8], mode: u32) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.as_old_mut().linkname[..link.len()].copy_from_slice(link);
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        let mut out = header.as_bytes().to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    /// A `.tar.gz` of raw entries (and the end-of-archive blocks).
    fn raw(entries: &[Vec<u8>]) -> Vec<u8> {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        for e in entries {
            gz.write_all(e).unwrap();
        }
        gz.write_all(&[0; 1024]).unwrap();
        gz.finish().unwrap()
    }

    /// Refused, with nothing left in `dir` (the staging directory is gone, no file escaped).
    fn refused(archive: &[u8], dir: &Path, expect: &str) {
        let dest = dir.join("t");
        let err = install_dir(
            archive,
            "top",
            &dest,
            "toolchain",
            Existing::Replace,
            |_| None,
        )
        .unwrap_err();
        assert!(err.contains(expect), "expected `{expect}` in: {err}");
        let left: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[test]
    fn refuses_unsafe_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("in");
        std::fs::create_dir_all(&dir).unwrap();
        use tar::EntryType as K;
        let file = |name: &[u8]| entry(name, K::Regular, b"", b"x", 0o644);
        refused(
            &raw(&[file(b"other/kit.stamp")]),
            &dir,
            "everything must be under top/",
        );
        refused(
            &raw(&[file(b"/top/abs")]),
            &dir,
            "everything must be under top/",
        );
        refused(
            &raw(&[file(b"top/../../evil")]),
            &dir,
            "not a plain relative path",
        );
        refused(
            &raw(&[file(b"top/a/./../../evil")]),
            &dir,
            "not a plain relative path",
        );
        let only_files = "only files may be in a toolchain";
        refused(
            &raw(&[entry(b"top/l", K::Symlink, b"/etc/passwd", b"", 0o777)]),
            &dir,
            only_files,
        );
        refused(
            &raw(&[entry(b"top/h", K::Link, b"top/x", b"", 0o644)]),
            &dir,
            only_files,
        );
        refused(
            &raw(&[entry(b"top/c", K::Char, b"", b"", 0o644)]),
            &dir,
            only_files,
        );
        refused(
            &raw(&[entry(b"top/b", K::Block, b"", b"", 0o644)]),
            &dir,
            only_files,
        );
        refused(
            &raw(&[entry(b"top/f", K::Fifo, b"", b"", 0o644)]),
            &dir,
            only_files,
        );
        // A GNU long name (`././@LongLink`) hiding `..` past the 100-byte name field.
        let long = format!("top/{}/../../../evil\0", "d".repeat(120));
        let long_name = entry(
            b"././@LongLink",
            K::GNULongName,
            b"",
            long.as_bytes(),
            0o644,
        );
        refused(
            &raw(&[long_name, file(b"placeholder")]),
            &dir,
            "not a plain relative path",
        );
        // `top` itself as a file.
        refused(&raw(&[file(b"top")]), &dir, "top must be a directory");
        refused(b"not gzip", &dir, "the toolchain");
        assert!(!tmp.path().join("evil").exists() && !Path::new("/top").exists());

        // macOS metadata entries are skipped, not refused.
        let dest = dir.join("t");
        let meta = archive(&[("top/a", b"", 0o644), ("._top", b"meta", 0o644)]);
        install_dir(&meta, "top", &dest, "toolchain", Existing::Replace, |_| {
            None
        })
        .unwrap();
        assert!(dest.join("a").is_file());
    }

    #[test]
    fn unpacking_is_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let limits = UnpackLimits {
            entry: 10,
            total: 15,
        };
        let big = archive(&[("top/a", &[0; 11], 0o644)]);
        let err = unpack_within(&big, "top", &tmp.path().join("1"), "pack", limits).unwrap_err();
        assert!(err.contains("more data than a release holds"), "{err}");
        let many = archive(&[("top/a", &[0; 8], 0o644), ("top/b", &[0; 8], 0o644)]);
        let err = unpack_within(&many, "top", &tmp.path().join("2"), "pack", limits).unwrap_err();
        assert!(err.contains("top/b"), "{err}");
        let fits = archive(&[("top/a", &[0; 8], 0o644), ("top/b", &[0; 7], 0o644)]);
        unpack_within(&fits, "top", &tmp.path().join("3"), "pack", limits).unwrap();
    }

    /// Any mode with an execute bit becomes exactly 0o755 (no setuid, setgid, sticky or
    /// world-writable bits); others keep the default for a new file.
    #[cfg(unix)]
    #[test]
    fn executable_bits_and_nothing_else() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let a = archive(&[
            ("top/suid", b"", 0o4777),
            ("top/plain", b"", 0o666),
            ("top/exe", b"", 0o700),
        ]);
        unpack(&a, "top", tmp.path(), "toolchain").unwrap();
        let mode = |p: &str| {
            std::fs::metadata(tmp.path().join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode("suid"), 0o755);
        assert_eq!(mode("exe"), 0o755);
        assert_eq!(mode("plain") & 0o7111, 0, "{:o}", mode("plain"));
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
    fn a_version_installed_meanwhile_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("0.1.0");
        std::fs::create_dir_all(dest.join("bin")).unwrap();
        std::fs::write(dest.join("bin/velt"), b"first").unwrap();
        let a = archive(&[("top/bin/velt", b"second", 0o755)]);
        install_dir(&a, "top", &dest, "toolchain", Existing::Keep, |_| None).unwrap();
        assert_eq!(std::fs::read(dest.join("bin/velt")).unwrap(), b"first");
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "{left:?}");
        // Without one there, it is installed.
        let fresh = tmp.path().join("0.2.0");
        install_dir(&a, "top", &fresh, "toolchain", Existing::Keep, |_| None).unwrap();
        assert_eq!(std::fs::read(fresh.join("bin/velt")).unwrap(), b"second");
    }

    #[test]
    fn concurrent_installs_of_one_version_all_succeed() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("0.1.0");
        let a = archive(&[
            ("top/bin/velt", b"exe", 0o755),
            ("top/std/x", &[7; 4096], 0o644),
        ]);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    install_dir(&a, "top", &dest, "toolchain", Existing::Keep, |_| None).unwrap()
                });
            }
        });
        assert!(dest.join("std/x").is_file());
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "{left:?}");
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
