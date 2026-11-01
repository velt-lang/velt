//! The machine's build cache (`velt_build_cache`) as the build uses it: where it is, how large it
//! may grow, and the backends' view of it.
//!
//! `$VELT_BUILD_CACHE`: the directory (default `$VELT_HOME/build-cache`; for a `velt` that cargo
//! built, `velt-build-cache` in its target directory), or `off` to build without it.
//! `$VELT_BUILD_CACHE_SIZE`: its size limit (`2G` by default; `500M`, `10G`, ...).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use velt_build_cache::Cache;

/// The cache this process uses, `None` when it is turned off or there is no home directory.
pub fn cache() -> Option<&'static Cache> {
    static CACHE: OnceLock<Option<Cache>> = OnceLock::new();
    CACHE.get_or_init(open).as_ref()
}

fn open() -> Option<Cache> {
    let root = root()?;
    Some(Cache::new(&root, limit()))
}

/// Who compiles: this velt's version line and its executable's size and modification time (a
/// rebuilt development velt is another compiler). Every key of compiled code, the build stamp
/// and the warm-up marker include it.
pub fn compiler_identity() -> velt_build_cache::Key {
    let mut h = velt_build_cache::KeyHasher::new();
    h.part(crate::commands::version::version_line().as_bytes());
    let meta = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::metadata(e).ok());
    if let Some(meta) = meta {
        h.part(&meta.len().to_le_bytes());
        let time = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        h.part(&time.unwrap_or_default().as_nanos().to_le_bytes());
    }
    h.finish()
}

/// The cache's directory, `None` when it is turned off.
pub fn root() -> Option<PathBuf> {
    match std::env::var_os("VELT_BUILD_CACHE") {
        Some(v) if is_off(&v.to_string_lossy()) => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => {
            let exe = std::env::current_exe().ok();
            match exe.as_deref().and_then(cargo_target_dir) {
                Some(target) => Some(target.join("velt-build-cache")),
                None => vpm::locations::velt_home()
                    .ok()
                    .map(|home| home.join("build-cache")),
            }
        }
    }
}

/// The cargo target directory `exe` was built into (`<target>/<profile>/velt`): the one holding
/// cargo's `.rustc_info.json`, or whose profile directory holds its `.cargo-lock` (cargo always
/// writes both; its `CACHEDIR.TAG` only into a directory it created itself). Builds of a source
/// checkout keep their cache beside their binaries, wherever `CARGO_TARGET_DIR` puts them, not
/// in the user's home.
fn cargo_target_dir(exe: &Path) -> Option<PathBuf> {
    let profile = exe.parent()?;
    let target = profile.parent()?;
    let cargo = target.join(".rustc_info.json").is_file() || profile.join(".cargo-lock").is_file();
    cargo.then(|| target.to_path_buf())
}

pub fn is_off(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "off" | "0" | "false" | "no" | "none"
    )
}

/// `$VELT_BUILD_CACHE_SIZE`, else the default.
pub fn limit() -> u64 {
    std::env::var("VELT_BUILD_CACHE_SIZE")
        .ok()
        .and_then(|v| velt_build_cache::parse_size(&v))
        .unwrap_or(velt_build_cache::DEFAULT_LIMIT)
}

/// Cranelift's compiled functions (`velt_codegen_cl::CodeCache`).
pub struct Functions(pub &'static Cache);

impl velt_codegen_cl::CodeCache for Functions {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get("cl", key)
    }

    fn put(&self, key: &[u8], value: &[u8]) {
        self.0.put("cl", key, value);
    }
}

/// Objects clang compiled from LLVM IR.
pub struct Objects(pub &'static Cache);

impl velt_codegen_cl::CodeCache for Objects {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get("llvm", key)
    }

    fn put(&self, key: &[u8], value: &[u8]) {
        self.0.put("llvm", key, value);
    }
}

/// Keep the cache under its limit, after a build that stored entries (at most hourly).
pub fn collect_garbage_if_due() {
    if let Some(cache) = cache() {
        cache.collect_garbage_if_due();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_builds_keep_the_cache_in_their_target_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("debug/velt");
        assert_eq!(cargo_target_dir(&exe), None);
        // A target directory made before cargo ran has no `CACHEDIR.TAG`.
        std::fs::write(tmp.path().join(".rustc_info.json"), "{}").unwrap();
        assert_eq!(cargo_target_dir(&exe).as_deref(), Some(tmp.path()));
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join("release")).unwrap();
        std::fs::write(other.path().join("release/.cargo-lock"), "").unwrap();
        let exe = other.path().join("release/velt");
        assert_eq!(cargo_target_dir(&exe).as_deref(), Some(other.path()));
    }

    #[test]
    fn off_values() {
        assert!(is_off("off") && is_off(" OFF ") && is_off("0") && is_off("false"));
        assert!(!is_off("/tmp/cache"));
    }
}
