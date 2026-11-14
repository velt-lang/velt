//! What a registry serves, checked against what was locked and published: a locked version's
//! checksums ([`check_locked_checksums`]), the dependencies an index lists against the package's
//! own `package.velt` ([`check_listed`]), and a prebuilt library's native ABI ([`check_abi`]).

use std::collections::BTreeMap;
use std::path::Path;

use crate::locations::Locations;
use crate::lockfile::{Lockfile, LOCK_FILE, REGISTRY_SOURCE};
use crate::manifest::Manifest;
use crate::native::NATIVE_ABI;
use crate::resolve::ResolvedPackage;

/// The dependencies the registry's index `listed` for registry package `pkg` (and the resolution
/// followed) must be the ones its own `package.velt`, in the verified cache entry `dir`, declares.
/// The checksum in the index and the lock covers the package, not the index entry, so a registry
/// could otherwise add a dependency to a locked version, or change a requirement, unseen.
pub(super) fn check_listed(
    loc: &Locations,
    pkg: &ResolvedPackage,
    listed: &BTreeMap<String, String>,
    dir: &Path,
) -> Result<(), String> {
    let manifest = Manifest::from_dir(dir)?;
    let own = crate::registry::index_dependencies(&manifest);
    let same_req = |a: &str, b: &str| {
        a == b
            || matches!(
                (semver::VersionReq::parse(a), semver::VersionReq::parse(b)),
                (Ok(a), Ok(b)) if a == b
            )
    };
    let declared = match &own {
        Ok(own)
            if own.len() == listed.len()
                && listed
                    .iter()
                    .all(|(name, req)| own.get(name).is_some_and(|o| same_req(req, o))) =>
        {
            return Ok(())
        }
        Ok(own) => show_dependencies(own),
        Err(e) => e.clone(),
    };
    Err(format!(
        "the registry `{}` lists other dependencies for `{}` {} than its package.velt declares: \
         the index has {}, the package has {declared}. The checksum covers the package, not the \
         index, so it was not installed; report this to the registry's operators",
        loc.describe(),
        pkg.name,
        pkg.version,
        show_dependencies(listed),
    ))
}

/// `a ^1.0, b ~2.1`, or `none`.
fn show_dependencies(deps: &BTreeMap<String, String>) -> String {
    if deps.is_empty() {
        return "none".to_string();
    }
    deps.iter()
        .map(|(name, req)| format!("`{name}` {req}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A registry package's native libraries need ABI `abi`, which this velt must provide.
pub(super) fn check_abi(name: &str, version: &semver::Version, abi: u32) -> Result<(), String> {
    if abi > NATIVE_ABI {
        return Err(format!(
            "`{name} {version}` needs Velt native ABI {abi}; this velt provides {NATIVE_ABI}. Update velt."
        ));
    }
    Ok(())
}

/// Under `--locked`, a version that is still locked keeps exactly its locked native libraries
/// (targets the author added since are ignored).
pub(super) fn keep_locked_native(old: &Lockfile, new: &mut Lockfile) {
    for pkg in &mut new.packages {
        if let Some(locked) = old.get(&pkg.name) {
            if locked.version == pkg.version
                && locked.source == pkg.source
                && locked.checksum == pkg.checksum
            {
                pkg.native = locked.native.clone();
            }
        }
    }
}

/// A registry package whose version is unchanged must still have the locked content hash, and
/// each of its locked native libraries the locked hash.
pub(super) fn check_locked_checksums(old: &Lockfile, new: &Lockfile) -> Result<(), String> {
    for pkg in &new.packages {
        let Some(locked) = old.get(&pkg.name) else {
            continue;
        };
        if locked.version != pkg.version || locked.source != pkg.source {
            continue;
        }
        if locked.checksum != pkg.checksum {
            let now = match pkg.source == REGISTRY_SOURCE {
                true => "the registry has {} (the published package changed, or velt.lock.json was written against another registry: check $VELT_REGISTRY and `registry`)",
                false => "the fetched tree hashes to {} (it changed in the cache: delete it there to fetch it again)",
            };
            let now = now.replace("{}", pkg.checksum.as_deref().unwrap_or("none"));
            return Err(format!(
                "checksum mismatch for `{}` {}: {LOCK_FILE} has {}, {now}",
                pkg.name,
                pkg.version,
                locked.checksum.as_deref().unwrap_or("none"),
            ));
        }
        for (target, sum) in &locked.native {
            match pkg.native.get(target) {
                Some(now) if now == sum => {}
                now => {
                    return Err(format!(
                        "checksum mismatch for the {target} native library of `{}` {}: {LOCK_FILE} has {sum}, the registry has {} (the published library changed)",
                        pkg.name,
                        pkg.version,
                        now.map_or("none", |s| s.as_str()),
                    ))
                }
            }
        }
    }
    Ok(())
}
