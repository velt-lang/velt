//! The package cache: registry packages extracted to `<cache>/<name>-<version>/`, the directory the
//! compiler reads them from. For the local registry "extracting" is a copy; every fetch verifies the
//! content checksum so a tampered cache or registry entry is caught.

use std::path::PathBuf;

use semver::Version;

use crate::contents;
use crate::locations::Locations;

/// Ensure `name` `version` is in the cache with contents hashing to `checksum`; returns its dir.
/// A cached copy with the wrong checksum is replaced from the registry; a registry copy with the
/// wrong checksum is an error.
pub fn fetch(
    loc: &Locations,
    name: &str,
    version: &Version,
    checksum: &str,
) -> Result<PathBuf, String> {
    let dir = loc.cached_package(name, version);
    if dir.is_dir() && contents::checksum(&dir).is_ok_and(|sum| sum == checksum) {
        return Ok(dir);
    }
    if let Some(url) = &loc.remote {
        return crate::remote::download(url, name, version, checksum, &dir);
    }
    let published = loc.registry_package(name, version);
    if !published.is_dir() {
        return Err(format!(
            "`{name}` {version} is missing from the registry `{}`",
            loc.registry.display()
        ));
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot clean `{}`: {e}", dir.display()))?;
    }
    contents::copy_package(&published, &dir)?;
    let actual = contents::checksum(&dir)?;
    if actual != checksum {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!(
            "checksum mismatch for `{name}` {version}: expected {checksum}, registry contents hash to {actual}"
        ));
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetches_verifies_and_repairs() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = Locations::under(tmp.path());
        let pkg = tmp.path().join("p");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("velt.toml"),
            "[package]\nname = \"p\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.vlt"), "export const X: i64 = 1;\n").unwrap();
        let entry = crate::registry::publish(&pkg, &loc).unwrap();
        let v = Version::new(1, 0, 0);

        let dir = fetch(&loc, "p", &v, &entry.checksum).unwrap();
        assert_eq!(dir, loc.cache.join("p-1.0.0"));
        assert!(dir.join("src/lib.vlt").is_file());

        // A corrupted cache entry is replaced.
        std::fs::write(dir.join("src/lib.vlt"), "tampered").unwrap();
        fetch(&loc, "p", &v, &entry.checksum).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("src/lib.vlt")).unwrap(),
            "export const X: i64 = 1;\n"
        );

        let err = fetch(&loc, "p", &v, "sha256:bad").unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(fetch(&loc, "p", &Version::new(9, 0, 0), "x")
            .unwrap_err()
            .contains("missing"));
    }
}
