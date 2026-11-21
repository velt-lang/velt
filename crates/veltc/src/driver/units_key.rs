//! The key std's unit is kept on disk under (`VELT_UNITS_DISK`, velt_sema `units::disk`).

use super::{loader, CheckScope, Session};

/// With std checked as a unit of its own (`VELT_UNITS=2`) and kept on disk (`VELT_UNITS_DISK`),
/// the key of this compiler and std's sources that std's unit is kept under (velt_sema
/// `units::disk`): std's modules by path, so every program's load gives the same key.
pub(super) fn units_std_key(
    sess: &Session,
    scope: &CheckScope,
    loaded: &loader::Loaded,
) -> Option<u64> {
    let on = scope.units
        && std::env::var_os("VELT_UNITS").is_some_and(|v| v == "2")
        && std::env::var_os("VELT_UNITS_DISK").is_some();
    if !on {
        return None;
    }
    let mut std: Vec<&velt_sema::SourceModule> =
        loaded.modules.iter().filter(|m| m.is_std).collect();
    std.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = velt_build_cache::KeyHasher::new();
    h.part(&crate::build_cache::compiler_identity().0);
    for m in std {
        h.part(m.path.as_bytes());
        h.part(sess.sm.get(m.file).src.as_bytes());
    }
    let key = h.finish().0;
    Some(u64::from_le_bytes(key[..8].try_into().ok()?))
}
