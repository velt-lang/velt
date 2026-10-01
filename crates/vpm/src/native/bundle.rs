//! Native bundles as files: listing, the content checksum `velt.lock` pins, and the archive form
//! exchanged with a registry. Unpacking verifies the checksum before anything is written, so an
//! unverified library is never on disk where the compiler would load or link it.

use std::path::Path;

use crate::archive;
use crate::native::{NativeMeta, META_FILE};

/// Whether `path` (relative, `/`-separated) may be part of a bundle.
fn allowed(path: &str) -> bool {
    path == META_FILE
        || ["shared/", "static/"].iter().any(|d| {
            path.strip_prefix(d)
                .is_some_and(|f| !f.is_empty() && !f.contains('/'))
        })
}

/// The bundle's files relative to `dir`, sorted.
pub fn list_files(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.join(META_FILE).is_file() {
        return Err(format!(
            "`{}` is not a native bundle (no {META_FILE})",
            dir.display()
        ));
    }
    let mut files = vec![META_FILE.to_string()];
    for sub in ["shared", "static"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        for e in entries.flatten() {
            if e.path().is_file() {
                files.push(format!("{sub}/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn entries(dir: &Path) -> Result<Vec<archive::Entry>, String> {
    list_files(dir)?
        .into_iter()
        .map(|rel| {
            let path = dir.join(&rel);
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
            Ok(archive::Entry { path: rel, bytes })
        })
        .collect()
}

/// `sha256:<hex>` of the bundle at `dir` (the hash of [`crate::contents::checksum`]).
pub fn checksum(dir: &Path) -> Result<String, String> {
    archive::checksum_of(entries(dir)?)
}

/// The archive of the bundle at `dir`.
pub fn pack(dir: &Path) -> Result<Vec<u8>, String> {
    archive::pack_files(dir, &list_files(dir)?)
}

/// The checksum of a bundle archive.
pub fn archive_checksum(bytes: &[u8]) -> Result<String, String> {
    archive::checksum_of(archive::entries_with(bytes, allowed)?)
}

/// Check that a bundle archive hashes to `expected` and describes `name` `version` for `target`,
/// then write it to `dest` (replacing what was there). `what` names the source in errors.
pub fn unpack_verified(
    bytes: &[u8],
    expected: &str,
    (name, version, target): (&str, &str, &str),
    dest: &Path,
    what: &str,
) -> Result<(), String> {
    let entries = archive::entries_with(bytes, allowed)?;
    let actual = archive::checksum_of(entries.clone())?;
    if actual != expected {
        return Err(format!(
            "checksum mismatch for the {target} native library of `{name}` {version} from {what}: \
             expected {expected}, got {actual}"
        ));
    }
    let meta = entries
        .iter()
        .find(|e| e.path == META_FILE)
        .ok_or_else(|| format!("native library of `{name}` from {what} has no {META_FILE}"))?;
    let meta = NativeMeta::parse(&String::from_utf8_lossy(&meta.bytes), what)?;
    let files: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
    check_meta(&meta, (name, version, target), &files, what)?;
    if dest.exists() {
        std::fs::remove_dir_all(dest)
            .map_err(|e| format!("cannot clean `{}`: {e}", dest.display()))?;
    }
    archive::write_entries(entries, dest)
}

/// Copy the bundle at `from` to `to` (replaced).
pub fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        std::fs::remove_dir_all(to).map_err(|e| format!("cannot clean `{}`: {e}", to.display()))?;
    }
    archive::write_entries(entries(from)?, to)
}

/// A bundle's metadata must describe what it is used as, and name only its own (checksummed)
/// `files`: an absolute path or `../` in `shared`, `import_lib` or `static` would let the loader or
/// the linker use a file the checksum does not cover.
pub fn check_meta(
    meta: &NativeMeta,
    (name, version, target): (&str, &str, &str),
    files: &[String],
    what: &str,
) -> Result<(), String> {
    if meta.package != name || meta.version != version || meta.target != target {
        return Err(format!(
            "{what} is the native library of `{} {}` for {}, not of `{name} {version}` for {target}",
            meta.package, meta.version, meta.target
        ));
    }
    let named = [
        ("shared", Some(&meta.shared), "shared/"),
        ("import_lib", meta.import_lib.as_ref(), "shared/"),
        ("static", meta.static_obj.as_ref(), "static/"),
    ];
    for (key, path, dir) in named {
        let Some(path) = path else { continue };
        if !allowed(path) || !path.starts_with(dir) || !files.contains(path) {
            return Err(format!(
                "{what}: `{key} = \"{path}\"` in {META_FILE} must name a file of the bundle under `{dir}`"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub(crate) fn sample(dir: &Path, version: &str) {
        let meta = NativeMeta {
            package: "p".into(),
            version: version.into(),
            target: "x86_64-unknown-linux-gnu".into(),
            abi: 1,
            shared: "shared/libvelt_native_p.so".into(),
            import_lib: None,
            static_obj: Some("static/p.o".into()),
            exports: BTreeMap::new(),
        };
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::create_dir_all(dir.join("static")).unwrap();
        std::fs::write(dir.join(META_FILE), meta.to_toml()).unwrap();
        std::fs::write(dir.join("shared/libvelt_native_p.so"), b"ELF").unwrap();
        std::fs::write(dir.join("static/p.o"), b"obj").unwrap();
    }

    #[test]
    fn pack_verify_unpack() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        sample(&b, "1.0.0");
        std::fs::write(b.join("stray.txt"), "not part of the bundle").unwrap();
        let sum = checksum(&b).unwrap();
        let bytes = pack(&b).unwrap();
        assert_eq!(archive_checksum(&bytes).unwrap(), sum);

        let id = ("p", "1.0.0", "x86_64-unknown-linux-gnu");
        let out = tmp.path().join("out");
        unpack_verified(&bytes, &sum, id, &out, "test").unwrap();
        assert_eq!(checksum(&out).unwrap(), sum);
        assert!(!out.join("stray.txt").exists());

        // Wrong checksum: nothing is written.
        let other = tmp.path().join("other");
        let e = unpack_verified(&bytes, "sha256:00", id, &other, "test").unwrap_err();
        assert!(e.contains("checksum mismatch"), "{e}");
        assert!(!other.exists());

        // Metadata for another target is refused.
        let e = unpack_verified(
            &bytes,
            &sum,
            ("p", "1.0.0", "aarch64-apple-darwin"),
            &other,
            "t",
        )
        .unwrap_err();
        assert!(
            e.contains("not of `p 1.0.0` for aarch64-apple-darwin"),
            "{e}"
        );
    }

    #[test]
    fn metadata_may_only_name_its_own_files() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        sample(&b, "1.0.0");
        let files = list_files(&b).unwrap();
        let good = NativeMeta::read(&b).unwrap();
        let id = ("p", "1.0.0", "x86_64-unknown-linux-gnu");
        check_meta(&good, id, &files, "t").unwrap();
        for (shared, static_obj) in [
            ("/etc/passwd", None),
            ("../../libevil.so", None),
            ("shared/../../x.so", None),
            ("shared/missing.so", None),
            ("static/p.o", None),
            (
                "shared/libvelt_native_p.so",
                Some("shared/libvelt_native_p.so"),
            ),
            ("shared/libvelt_native_p.so", Some("/tmp/evil.o")),
        ] {
            let mut bad = good.clone();
            bad.shared = shared.into();
            bad.static_obj = static_obj.map(Into::into);
            let e = check_meta(&bad, id, &files, "t").unwrap_err();
            assert!(
                e.contains("must name a file of the bundle"),
                "{shared}: {e}"
            );
        }

        // A malicious native.toml inside a correctly checksummed archive is refused on unpack.
        let mut evil = good.clone();
        evil.shared = "/usr/lib/libc.so.6".into();
        std::fs::write(b.join(META_FILE), evil.to_toml()).unwrap();
        let sum = checksum(&b).unwrap();
        let out = tmp.path().join("out");
        let e = unpack_verified(&pack(&b).unwrap(), &sum, id, &out, "t").unwrap_err();
        assert!(e.contains("must name a file of the bundle"), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn rejects_paths_outside_the_layout() {
        assert!(allowed("shared/libx.so") && allowed("static/x.o") && allowed(META_FILE));
        assert!(!allowed("shared/") && !allowed("shared/a/b") && !allowed("src/x.vlt"));
    }
}
