//! Native bundles as files: listing, the content checksum `velt.lock.json` pins, and the archive
//! form exchanged with a registry. Unpacking verifies the checksum before anything is written, so
//! an unverified library is never on disk where the compiler would load or link it.

use std::path::Path;

use crate::archive;
use crate::native::{
    export_prefix, exports, init_symbol, library_files, NativeMeta, LEGACY_META_FILE, META_FILE,
};

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
    let library = entries
        .iter()
        .find(|e| e.path == meta.shared)
        .ok_or_else(|| format!("{what}: the bundle has no `{}`", meta.shared))?;
    check_exports(&meta, &library.bytes, what)?;
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
/// the linker use a file the checksum does not cover. The libraries must carry the package's own
/// names ([`library_files`]): on Windows the DLL is copied next to the executable, where a bundle
/// naming another package's DLL would replace it.
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
    check_library_files(meta, what)?;
    check_export_names(meta, what)
}

/// Every export listed in the metadata carries the package's prefix (`<pkg>_`): a `declare` of a
/// listed name must resolve to the package's own library, never to a C library function such as
/// `free` that an unprefixed name would bind to.
fn check_export_names(meta: &NativeMeta, what: &str) -> Result<(), String> {
    let prefix = export_prefix(&meta.package);
    let init = init_symbol(&meta.package);
    let bad: Vec<&str> = meta
        .exports
        .keys()
        .filter(|n| !n.starts_with(&prefix) || **n == init)
        .map(String::as_str)
        .collect();
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{what}: {META_FILE} lists exports that are not package `{}`'s functions (each must start with `{prefix}`): `{}`",
        meta.package,
        bad.join("`, `")
    ))
}

/// The shared library (`library`: its bytes) must export exactly the functions, with the
/// signatures, that the metadata lists: a prebuilt bundle's list is not trusted as written.
pub fn check_exports(meta: &NativeMeta, library: &[u8], what: &str) -> Result<(), String> {
    let actual = exports::read(library, &meta.package)
        .map_err(|e| format!("{what}: the shared library `{}`: {e}", meta.shared))?;
    if actual == meta.exports {
        return Ok(());
    }
    let mut problems = vec![];
    for (name, sig) in &meta.exports {
        match actual.get(name) {
            None => problems.push(format!(
                "`{name}` is listed but the library does not export it"
            )),
            Some(real) if real != sig => problems.push(format!(
                "`{name}` is listed as `{sig}`, but the library records `{real}`"
            )),
            Some(_) => {}
        }
    }
    for name in actual.keys().filter(|n| !meta.exports.contains_key(*n)) {
        problems.push(format!("the library exports `{name}`, which is not listed"));
    }
    Err(format!(
        "{what}: {META_FILE} does not match the shared library `{}`:\n  {}",
        meta.shared,
        problems.join("\n  ")
    ))
}

fn check_library_files(meta: &NativeMeta, what: &str) -> Result<(), String> {
    let (shared, import_lib) = library_files(&meta.package, &meta.target);
    let named = [
        ("shared", Some(&meta.shared), Some(shared)),
        ("import_lib", meta.import_lib.as_ref(), import_lib),
    ];
    for (key, path, expected) in named {
        let Some(path) = path else { continue };
        let file = path.strip_prefix("shared/").unwrap_or(path);
        if expected.as_deref() != Some(file) {
            let must = match expected {
                Some(e) => format!("be `shared/{e}`"),
                None => format!("not be set for {}", meta.target),
            };
            return Err(format!(
                "{what}: `{key} = \"{path}\"` in {META_FILE} must {must} (the library of package `{}`)",
                meta.package
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
            exports: BTreeMap::from([("p_open".into(), "()->u64".into())]),
        };
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::create_dir_all(dir.join("static")).unwrap();
        std::fs::write(dir.join(META_FILE), meta.to_json()).unwrap();
        let library = exports::sample_library("p", &meta.exports, version);
        std::fs::write(dir.join("shared/libvelt_native_p.so"), library).unwrap();
        std::fs::write(dir.join("static/p.o"), b"obj").unwrap();
    }

    /// `sample`'s bundle with its metadata's export list replaced by `exports`.
    fn with_listed_exports(dir: &Path, exports: &[(&str, &str)]) {
        let mut meta = NativeMeta::read(dir).unwrap();
        meta.exports = exports
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect();
        std::fs::write(dir.join(META_FILE), meta.to_json()).unwrap();
    }

    #[test]
    fn listed_exports_must_be_the_librarys_own() {
        let tmp = tempfile::tempdir().unwrap();
        let id = ("p", "1.0.0", "x86_64-unknown-linux-gnu");
        let b = tmp.path().join("b");
        sample(&b, "1.0.0");
        crate::native::NativeLib::open(&b, crate::native::NativeOrigin::Prebuilt).unwrap();
        let cases: [(&[(&str, &str)], &str); 4] = [
            // A C library function: a `declare` of it would bind to libc.
            (
                &[("p_open", "()->u64"), ("free", "(u64)->void")],
                "(each must start with `p_`): `free`",
            ),
            (
                &[("p_open", "()->u64"), ("p_close", "(u64)->void")],
                "`p_close` is listed but the library does not export it",
            ),
            (
                &[("p_open", "(u64)->u64")],
                "`p_open` is listed as `(u64)->u64`, but the library records `()->u64`",
            ),
            (&[], "the library exports `p_open`, which is not listed"),
        ];
        for (listed, expected) in cases {
            with_listed_exports(&b, listed);
            let open = crate::native::NativeLib::open(&b, crate::native::NativeOrigin::Prebuilt);
            let e = open.map(drop).unwrap_err();
            assert!(e.contains(expected), "{e}");
            let (bytes, sum) = (pack(&b).unwrap(), checksum(&b).unwrap());
            let out = tmp.path().join("out");
            let e = unpack_verified(&bytes, &sum, id, &out, "t").unwrap_err();
            assert!(e.contains(expected), "{e}");
            assert!(!out.exists());
        }
        // A library that is not one.
        sample(&b, "1.0.0");
        std::fs::write(b.join("shared/libvelt_native_p.so"), b"ELF").unwrap();
        let open = crate::native::NativeLib::open(&b, crate::native::NativeOrigin::Prebuilt);
        let e = open.map(drop).unwrap_err();
        assert!(
            e.contains("the shared library `shared/libvelt_native_p.so`: cannot read"),
            "{e}"
        );
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
    fn a_bundle_with_the_former_native_toml_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        sample(&b, "1.0.0");
        std::fs::rename(b.join(META_FILE), b.join(LEGACY_META_FILE)).unwrap();
        let e = list_files(&b).unwrap_err();
        assert!(e.contains("(the file is now `native.json`)"), "{e}");
        assert!(e.contains("velt native build"), "{e}");

        let files = [LEGACY_META_FILE.to_string(), "static/p.o".to_string()];
        let bytes = archive::pack_files(&b, &files).unwrap();
        let id = ("p", "1.0.0", "x86_64-unknown-linux-gnu");
        let out = tmp.path().join("out");
        let e = unpack_verified(&bytes, "sha256:00", id, &out, "test").unwrap_err();
        assert!(e.contains("built by an older velt"), "{e}");
        assert!(e.contains("now `native.json`"), "{e}");
        assert!(!out.exists());
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

        // A library named after another package (which on Windows would replace that package's
        // DLL next to the executable) is refused, even though it is a file of the bundle.
        let other = b.join("shared/libvelt_native_q.so");
        std::fs::write(&other, b"ELF").unwrap();
        let files = list_files(&b).unwrap();
        let mut bad = good.clone();
        bad.shared = "shared/libvelt_native_q.so".into();
        let e = check_meta(&bad, id, &files, "t").unwrap_err();
        assert!(e.contains("must be `shared/libvelt_native_p.so`"), "{e}");
        let mut bad = good.clone();
        bad.import_lib = Some("shared/libvelt_native_q.so".into());
        let e = check_meta(&bad, id, &files, "t").unwrap_err();
        assert!(e.contains("must not be set"), "{e}");
        std::fs::remove_file(other).unwrap();

        let win = ("p", "1.0.0", "x86_64-pc-windows-msvc");
        let names = ["shared/velt_native_p.dll", "shared/velt_native_p.dll.lib"];
        let names = names.map(String::from);
        let mut dll = good.clone();
        dll.target = win.2.into();
        dll.shared = names[0].clone();
        dll.import_lib = Some(names[1].clone());
        dll.static_obj = None;
        check_meta(&dll, win, &names, "t").unwrap();
        for (shared, import) in [
            ("shared/velt_native_q.dll", "shared/velt_native_p.dll.lib"),
            ("shared/velt_native_p.dll", "shared/velt_native_q.dll.lib"),
        ] {
            let mut bad = dll.clone();
            bad.shared = shared.into();
            bad.import_lib = Some(import.into());
            let files = [shared.to_string(), import.to_string()];
            let e = check_meta(&bad, win, &files, "t").unwrap_err();
            assert!(e.contains("(the library of package `p`)"), "{shared}: {e}");
        }

        // A malicious native.json inside a correctly checksummed archive is refused on unpack.
        let mut evil = good.clone();
        evil.shared = "/usr/lib/libc.so.6".into();
        std::fs::write(b.join(META_FILE), evil.to_json()).unwrap();
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
