//! Where the C libraries a package builds from `sources` come from at install
//! (docs/internals/design/c-ffi.md "Where libraries come from"): a published bundle for the
//! target, as a Rust crate's, else a directory to build them into when a program links them.
//! Nothing here runs zig.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::graph::{CBundle, GraphPackage};
use crate::locations::Locations;
use crate::manifest::Manifest;
use crate::native::{missing_c_target_message, NativeLib};

use super::{from_source_allowed, FROM_SOURCE_VAR};

/// The native libraries of an install, by package.
#[derive(Debug, Default)]
pub(super) struct Natives {
    /// Prebuilt or built here: a Rust crate's, or a bundle of C libraries that was published.
    pub(super) libs: BTreeMap<String, NativeLib>,
    /// Where a package's C `sources` build to.
    pub(super) c_sources: BTreeMap<String, PathBuf>,
}

impl Natives {
    /// Give `package` what this install has for `name`.
    pub(super) fn attach(&mut self, package: &mut GraphPackage, name: &str) {
        match self.libs.remove(name) {
            Some(lib) if lib.has_crate() => package.native = Some(lib),
            Some(lib) => package.c_bundle = Some(CBundle::Prebuilt(Box::new(lib))),
            None => {}
        }
        if let Some(out) = self.c_sources.remove(name) {
            package.c_bundle = Some(CBundle::FromSource(out));
        }
    }
}

/// Whether C sources can be built for `target` at all: WebAssembly has no C library here, and a
/// call on it is an error where it is lowered.
fn buildable(target: &str) -> bool {
    velt_toolchain::zig::build_target(target).is_some()
}

/// A path package's (or the root's) bundle directory for `target`, as for its crate.
pub(super) fn local_out(dir: &Path, manifest: &Manifest, target: &str) -> Option<PathBuf> {
    (manifest.has_c_sources() && buildable(target))
        .then(|| dir.join("target").join("velt-native").join(target))
}

/// A git or url package's (`from`: how its source reads): only when the user asks for it, as for
/// a crate. Compiling C runs none of its code, but it does download zig and take a build this
/// machine alone made.
pub(super) fn remote_out(
    name: &str,
    from: &str,
    manifest: &Manifest,
    work: &Path,
    target: &str,
) -> Result<Option<PathBuf>, String> {
    if !manifest.has_c_sources() || !buildable(target) {
        return Ok(None);
    }
    if !from_source_allowed() {
        return Err(format!(
            "`{name}` from {from} has C sources, which it builds here with the pinned zig: set {FROM_SOURCE_VAR}=1 to allow it"
        ));
    }
    Ok(Some(work.join(target)))
}

/// A registry package's, when none was published for `target` and the user asks to build it.
pub(super) fn registry_out(
    loc: &Locations,
    (name, version): (&str, &semver::Version),
    dir: &Path,
    published: &BTreeMap<String, String>,
    target: &str,
) -> Result<Option<PathBuf>, String> {
    let manifest = Manifest::from_dir(dir)?;
    if !manifest.has_c_sources() || !buildable(target) || published.contains_key(target) {
        return Ok(None);
    }
    if !from_source_allowed() {
        let version = version.to_string();
        return Err(missing_c_target_message(name, &version, target, published));
    }
    let root = loc.cached_native_root(name, version);
    Ok(Some(root.join(format!("{target}-source"))))
}
