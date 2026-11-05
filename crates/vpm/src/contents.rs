//! The contents of a package as distributed: `package.velt`, everything under `src/`, the record
//! of its C declarations' header check (`c-headers.json`, [`crate::c_headers`]), and for a
//! package with a `native` object the sources of its crate, or of its C libraries (without
//! `target/` and `.git/`).
//! Listing, copying and content-hashing all use the same file set so a checksum computed on a
//! source tree matches the one computed on its registry or cache copy.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::{legacy, LEGACY_MANIFEST_FILE, MANIFEST_FILE, OLD_MANIFEST_FILE, SRC_DIR};

/// Package files relative to `root`, `/`-separated, sorted (deterministic hashing).
pub fn list_files(root: &Path) -> Result<Vec<String>, String> {
    let manifest = root.join(MANIFEST_FILE);
    if !manifest.is_file() && root.join(LEGACY_MANIFEST_FILE).is_file() {
        return Err(format!("`{}`: {}", root.display(), legacy::REPUBLISH));
    }
    if !manifest.is_file() && root.join(OLD_MANIFEST_FILE).is_file() {
        let why = legacy::republish_old_name();
        return Err(format!("`{}`: {why}", root.display()));
    }
    if !manifest.is_file() {
        return Err(format!("`{}` has no {MANIFEST_FILE}", root.display()));
    }
    let mut files = vec![MANIFEST_FILE.to_string()];
    if root.join(crate::c_headers::FILE).is_file() {
        files.push(crate::c_headers::FILE.to_string());
    }
    let src = root.join(SRC_DIR);
    if src.is_dir() {
        collect(&src, SRC_DIR, &mut files)?;
    }
    if let Some(dir) = native_dir(root) {
        let native = root.join(&dir);
        if native.is_dir() {
            collect(&native, &dir, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

/// The directory of the native sources of the package at `root`, if it has one (an unreadable
/// manifest has none here; it is reported where it is parsed).
fn native_dir(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(MANIFEST_FILE)).ok()?;
    native_dir_of(&text)
}

/// The directory of manifest text's native sources: `native.path`, or where C `sources` are.
pub fn native_dir_of(manifest: &str) -> Option<String> {
    let manifest = crate::manifest::Manifest::parse(manifest).ok()?;
    match manifest.native {
        Some(native) => Some(native.path),
        None if manifest.has_c_sources() => Some(crate::manifest::C_SOURCES_DIR.to_string()),
        None => None,
    }
}

/// Directories never distributed inside a native crate.
const SKIPPED_DIRS: &[&str] = &["target", ".git"];

fn collect(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let child = format!("{rel}/{name}");
        if path.is_dir() {
            if SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect(&path, &child, out)?;
        } else {
            out.push(child);
        }
    }
    Ok(())
}

/// `sha256:<hex>` over the package files: for each file, its relative path, a NUL, its length
/// and its bytes. Line endings are hashed as stored.
pub fn checksum(root: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for rel in list_files(root)? {
        let bytes = read(&root.join(&rel))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

/// Copy the package files of `from` into `to` (created; must not contain other files that matter).
pub fn copy_package(from: &Path, to: &Path) -> Result<(), String> {
    for rel in list_files(from)? {
        let dest: PathBuf = to.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
        }
        std::fs::copy(from.join(&rel), &dest)
            .map_err(|e| format!("cannot copy to `{}`: {e}", dest.display()))?;
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn lists_hashes_and_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        write(&a, "package.velt", "");
        write(&a, "src/lib.velt", "export function f() {}");
        write(&a, "src/sub/x.velt", "");
        write(&a, "target/junk", "ignored");
        write(&a, "c-headers.json", "{}");
        assert_eq!(
            list_files(&a).unwrap(),
            [
                "c-headers.json",
                "package.velt",
                "src/lib.velt",
                "src/sub/x.velt"
            ]
        );

        let b = tmp.path().join("b");
        copy_package(&a, &b).unwrap();
        assert!(!b.join("target").exists());
        let sum = checksum(&a).unwrap();
        assert!(sum.starts_with("sha256:") && sum.len() == 7 + 64);
        assert_eq!(checksum(&b).unwrap(), sum);

        write(&b, "src/lib.velt", "export function g() {}");
        assert_ne!(checksum(&b).unwrap(), sum);
    }

    #[test]
    fn native_crate_sources_are_included() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path();
        write(
            a,
            "package.velt",
            "export const pkg: Package = { name: \"a\", version: \"1.0.0\", native: { path: \"rs\" } };",
        );
        write(a, "src/lib.velt", "");
        write(a, "rs/Cargo.toml", "");
        write(a, "rs/src/lib.rs", "");
        write(a, "rs/target/release/libx.so", "");
        write(a, "native/ignored.rs", "");
        assert_eq!(
            list_files(a).unwrap(),
            [
                "package.velt",
                "rs/Cargo.toml",
                "rs/src/lib.rs",
                "src/lib.velt"
            ]
        );
    }

    #[test]
    fn missing_manifest_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_files(tmp.path()).unwrap_err().contains("package.velt"));
        // A package copy from before `package.velt` (in a registry or the cache).
        write(tmp.path(), "velt.toml", "");
        let err = list_files(tmp.path()).unwrap_err();
        assert!(err.contains("must publish a new version"), "{err}");
    }
}
