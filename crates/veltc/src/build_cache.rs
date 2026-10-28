//! The machine's build cache (`velt_build_cache`) as the build uses it: where it is, how large it
//! may grow, and the backends' view of it.
//!
//! `$VELT_BUILD_CACHE`: the directory (default `$VELT_HOME/build-cache`), or `off` to build without
//! it. `$VELT_BUILD_CACHE_SIZE`: its size limit (`2G` by default; `500M`, `10G`, ...).

use std::path::PathBuf;
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

/// The cache's directory, `None` when it is turned off.
pub fn root() -> Option<PathBuf> {
    match std::env::var_os("VELT_BUILD_CACHE") {
        Some(v) if is_off(&v.to_string_lossy()) => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => vpm::locations::velt_home()
            .ok()
            .map(|home| home.join("build-cache")),
    }
}

fn is_off(v: &str) -> bool {
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
    fn off_values() {
        assert!(is_off("off") && is_off(" OFF ") && is_off("0") && is_off("false"));
        assert!(!is_off("/tmp/cache"));
    }
}
