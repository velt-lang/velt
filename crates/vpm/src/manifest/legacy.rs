//! The former manifest, `velt.toml`, is no longer read. A package that still has one gets an error
//! that prints the equivalent `package.vlt` (docs/internals/design/package-manifest.md
//! "Migration"). This module is temporary: it goes away once nobody needs the conversion.

use std::path::Path;

use super::{Manifest, LEGACY_MANIFEST_FILE, MANIFEST_FILE};

/// Why a package version published with a `velt.toml` cannot be installed.
pub const REPUBLISH: &str = "it was published with a `velt.toml`, which this velt no longer \
reads (the manifest is `package.vlt`); its author must publish a new version";

/// Decode `velt.toml` text into a [`Manifest`]. Nothing is validated: the converted file is,
/// when it is read as `package.vlt`.
pub fn from_toml(src: &str) -> Result<Manifest, String> {
    toml::from_str(src).map_err(|e| format!("invalid {LEGACY_MANIFEST_FILE}: {e}"))
}

/// The error for a package at `dir` that has a `velt.toml` and no `package.vlt`.
pub fn migration_error(dir: &Path) -> String {
    let legacy = dir.join(LEGACY_MANIFEST_FILE);
    let head = format!(
        "`{}` is no longer read; the manifest is `{MANIFEST_FILE}`",
        legacy.display()
    );
    let converted = std::fs::read_to_string(&legacy)
        .map_err(|e| e.to_string())
        .and_then(|src| from_toml(&src));
    match converted {
        Ok(m) => format!(
            "{head}\n  = help: save this as `{}` and delete `{LEGACY_MANIFEST_FILE}` \
             (comments are not carried over):\n\n{}",
            dir.join(MANIFEST_FILE).display(),
            m.to_vlt()
        ),
        Err(e) => format!(
            "{head}\n  = note: it could not be converted ({e}); see docs/tooling/manifest.md"
        ),
    }
}
