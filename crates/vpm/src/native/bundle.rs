//! Native bundles as files: listing, the content checksum `velt.lock.json` pins, and the archive
//! form exchanged with a registry. Unpacking verifies the checksum before anything is written, so
//! an unverified library is never on disk where the compiler would load or link it.

use std::path::Path;

use crate::archive;
use crate::native::{c_library_files, NativeMeta, LEGACY_META_FILE, META_FILE, NATIVE_ABI};

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
        let old = dir.join(LEGACY_META_FILE);
        if old.is_file() {
            return Err(crate::json_file::legacy_error(
                &old,
                META_FILE,
                "rebuild the bundle with `velt native build`",
            ));
        }
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
    let entries = archive::entries_with(bytes, allowed).map_err(|e| {
        if e.contains(&format!("`{LEGACY_META_FILE}`")) {
            format!(
                "the {target} native library of `{name}` {version} from {what} was built by an older velt (it has a `{LEGACY_META_FILE}`; the metadata is now `{META_FILE}`): its author must publish it again"
            )
        } else {
            e
        }
    })?;
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
/// `files`: each C library under the names of [`c_library_files`], for its package and target
/// only, and every file there, so what the linker and the loader use is what the checksum covers,
/// and a DLL copied beside an executable can't replace another package's. A bundle without C
/// libraries is a Rust crate's (the old native ABI), which velt no longer loads.
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
    if meta.c.is_empty() || meta.abi > NATIVE_ABI || !meta.exports.is_empty() {
        return Err(format!(
            "{what} is a Rust crate's native library (the old native ABI), which velt no longer supports: its package must declare its C functions with `declare c` instead"
        ));
    }
    for (lib, actual) in &meta.c {
        let name_ok = !lib.is_empty()
            && lib
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !name_ok {
            return Err(format!(
                "{what}: `{lib}` in {META_FILE} is not a C library name"
            ));
        }
        let expected = c_library_files(&meta.package, lib, &meta.target);
        if *actual != expected {
            return Err(format!(
                "{what}: C library `{lib}` in {META_FILE} must have the files {expected:?}, not {actual:?}"
            ));
        }
        let all = [
            Some(&actual.shared),
            actual.import_lib.as_ref(),
            actual.static_lib.as_ref(),
        ];
        if let Some(missing) = all.into_iter().flatten().find(|f| !files.contains(f)) {
            return Err(format!(
                "{what}: C library `{lib}` has no `{missing}` in the bundle"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A Rust crate's bundle, as velt wrote them for the old native ABI: refused where it is
    /// opened and where it is unpacked, before anything is written.
    #[test]
    fn a_rust_crates_bundle_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        std::fs::create_dir_all(b.join("shared")).unwrap();
        std::fs::create_dir_all(b.join("static")).unwrap();
        std::fs::write(b.join("shared/libvelt_native_p.so"), b"ELF").unwrap();
        std::fs::write(b.join("static/p.o"), b"ELF").unwrap();
        let meta = r#"{
  "package": "p",
  "version": "1.0.0",
  "target": "x86_64-unknown-linux-gnu",
  "abi": 3,
  "shared": "shared/libvelt_native_p.so",
  "static": "static/p.o",
  "exports": {
    "velt_p__open": "()->u64"
  }
}
"#;
        std::fs::write(b.join(META_FILE), meta).unwrap();
        let open = crate::native::NativeLib::open(&b, crate::native::NativeOrigin::Prebuilt);
        let e = open.map(drop).unwrap_err();
        assert!(e.contains("a Rust crate's native library"), "{e}");
        let id = ("p", "1.0.0", "x86_64-unknown-linux-gnu");
        let (bytes, sum) = (pack(&b).unwrap(), checksum(&b).unwrap());
        let out = tmp.path().join("out");
        let e = unpack_verified(&bytes, &sum, id, &out, "t").unwrap_err();
        assert!(e.contains("a Rust crate's native library"), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn pack_verify_unpack() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        c_bundle(&b, "x86_64-unknown-linux-gnu");
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
    fn a_bundle_with_the_former_native_toml_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        c_bundle(&b, "x86_64-unknown-linux-gnu");
        std::fs::rename(b.join(META_FILE), b.join(LEGACY_META_FILE)).unwrap();
        let e = list_files(&b).unwrap_err();
        assert!(e.contains("(the file is now `native.json`)"), "{e}");
        assert!(e.contains("velt native build"), "{e}");

        let files = [
            LEGACY_META_FILE.to_string(),
            "static/libvelt_c_p__tiny.a".to_string(),
        ];
        let bytes = archive::pack_files(&b, &files).unwrap();
        let id = ("p", "1.0.0", "x86_64-unknown-linux-gnu");
        let out = tmp.path().join("out");
        let e = unpack_verified(&bytes, "sha256:00", id, &out, "test").unwrap_err();
        assert!(e.contains("built by an older velt"), "{e}");
        assert!(e.contains("now `native.json`"), "{e}");
        assert!(!out.exists());
    }

    #[test]
    fn rejects_paths_outside_the_layout() {
        assert!(allowed("shared/libx.so") && allowed("static/x.o") && allowed(META_FILE));
        assert!(!allowed("shared/") && !allowed("shared/a/b") && !allowed("src/x.velt"));
    }

    /// A bundle of C libraries for `target`, as `c_build` writes it.
    fn c_bundle(dir: &Path, target: &str) -> NativeMeta {
        let files = c_library_files("p", "tiny", target);
        let meta = NativeMeta {
            package: "p".into(),
            version: "1.0.0".into(),
            target: target.into(),
            abi: 0,
            exports: BTreeMap::new(),
            c: BTreeMap::from([("tiny".to_string(), files.clone())]),
        };
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::create_dir_all(dir.join("static")).unwrap();
        std::fs::write(dir.join(META_FILE), meta.to_json()).unwrap();
        let all = [
            Some(&files.shared),
            files.import_lib.as_ref(),
            files.static_lib.as_ref(),
        ];
        for file in all.into_iter().flatten() {
            std::fs::write(dir.join(file), b"library").unwrap();
        }
        meta
    }

    #[test]
    fn a_bundle_of_c_libraries_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
            let id = ("p", "1.0.0", target);
            let b = tmp.path().join(target);
            let meta = c_bundle(&b, target);
            let lib =
                crate::native::NativeLib::open(&b, crate::native::NativeOrigin::Prebuilt).unwrap();
            assert_eq!(lib.meta, meta);
            assert_eq!(NativeMeta::read(&b).unwrap(), meta);
            let sum = checksum(&b).unwrap();
            let dest = tmp.path().join("unpacked").join(target);
            unpack_verified(&pack(&b).unwrap(), &sum, id, &dest, "test").unwrap();
            assert_eq!(checksum(&dest).unwrap(), sum);
        }
    }

    /// `native.json` is not trusted for a C library's files: the linker and the loader take what
    /// it names, and a DLL goes beside the executable.
    #[test]
    fn a_c_library_names_only_its_own_files() {
        let tmp = tempfile::tempdir().unwrap();
        let target = "x86_64-pc-windows-msvc";
        let id = ("p", "1.0.0", target);
        let open = |meta: &NativeMeta, dir: &Path| {
            std::fs::write(dir.join(META_FILE), meta.to_json()).unwrap();
            check_meta(meta, id, &list_files(dir).unwrap(), "test")
        };
        let b = tmp.path().join("b");
        let good = c_bundle(&b, target);

        let mut escaping = good.clone();
        escaping.c.get_mut("tiny").unwrap().shared = "../../evil.dll".into();
        assert!(open(&escaping, &b)
            .unwrap_err()
            .contains("must have the files"));

        let mut others = good.clone();
        others.c.get_mut("tiny").unwrap().shared = "shared/velt_rt_shared.dll".into();
        assert!(open(&others, &b)
            .unwrap_err()
            .contains("must have the files"));

        let mut renamed = good.clone();
        let files = renamed.c.remove("tiny").unwrap();
        renamed.c.insert("../tiny".into(), files);
        assert!(open(&renamed, &b)
            .unwrap_err()
            .contains("is not a C library name"));

        let mut empty = good.clone();
        empty.c.clear();
        assert!(open(&empty, &b).unwrap_err().contains("a Rust crate's"));

        let mut exporting = good.clone();
        exporting
            .exports
            .insert("velt_p__x".into(), "()->u64".into());
        assert!(open(&exporting, &b).unwrap_err().contains("a Rust crate's"));

        let mut abi = good.clone();
        abi.abi = 3;
        assert!(open(&abi, &b).unwrap_err().contains("a Rust crate's"));

        std::fs::write(b.join(META_FILE), good.to_json()).unwrap();
        std::fs::remove_file(b.join(good.c["tiny"].import_lib.as_ref().unwrap())).unwrap();
        assert!(open(&good, &b).unwrap_err().contains("has no `shared/"));
    }
}
