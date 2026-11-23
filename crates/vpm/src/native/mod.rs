//! Native libraries of packages (docs/internals/design/c-ffi.md "Where libraries come from"): a
//! package whose C libraries are `sources` publishes them **prebuilt per target** as a *bundle*,
//! so users only need `velt`.
//!
//! A bundle is a directory (exchanged as an [`crate::archive`]):
//!
//! ```text
//! native.json                       # NativeMeta: package, version, target, the libraries
//! shared/libvelt_c_<pkg>__<lib>.so  # .dylib on macOS; velt_c_<pkg>__<lib>.dll + .dll.lib on Windows
//! static/libvelt_c_<pkg>__<lib>.a   # Linux/macOS
//! ```
//!
//! - [`bundle`]: the files, checksums, packing and verified unpacking.
//! - [`c_build`]: `zig cc` from C sources into a bundle (package authors; a target nobody
//!   published, when the user asks).
//!
//! A Rust crate's library (the old native ABI, with its runtime function table and export
//! signatures) is no longer supported (velt-lang/velt#2563): a bundle of one is refused.

pub mod bundle;
pub mod c_build;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The native ABI of the bundles this `velt` uses: 0, C libraries, which call no runtime function.
/// A bundle needing more is a Rust crate's library (the old native ABI), refused before download.
pub const NATIVE_ABI: u32 = 0;

/// File name of a bundle's metadata (generated JSON, [`crate::json_file`]).
pub const META_FILE: &str = "native.json";
/// The metadata's former (TOML) name, no longer read.
pub const LEGACY_META_FILE: &str = "native.toml";

/// `velt_c_<pkg>__<lib>`: the name of C library `lib` of `package` built from its `sources`.
/// Package names never contain `__`, so no two packages' libraries share one.
pub fn c_library_name(package: &str, lib: &str) -> String {
    format!("velt_c_{}__{lib}", package.replace('-', "_"))
}

/// The files of a C library built from `sources`: its shared library, on Windows its import
/// library, and elsewhere its static library for release builds.
pub fn c_library_files(package: &str, lib: &str, target: &str) -> CLibraryFiles {
    let name = c_library_name(package, lib);
    if target.contains("windows") {
        return CLibraryFiles {
            shared: format!("shared/{name}.dll"),
            import_lib: Some(format!("shared/{name}.dll.lib")),
            static_lib: None,
        };
    }
    let ext = if target.contains("apple") {
        "dylib"
    } else {
        "so"
    };
    CLibraryFiles {
        shared: format!("shared/lib{name}.{ext}"),
        import_lib: None,
        static_lib: Some(format!("static/lib{name}.a")),
    }
}

/// The contents of `native.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeMeta {
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Target triple the library was built for.
    pub target: String,
    /// The native ABI the libraries need: [`NATIVE_ABI`] (a Rust crate's library had more).
    pub abi: u32,
    /// A Rust crate's exports in the old native ABI: always empty (kept so that an older velt
    /// reads the bundle).
    pub exports: BTreeMap<String, String>,
    /// The C libraries built from the package's `sources`, by the name its declarations give.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub c: BTreeMap<String, CLibraryFiles>,
}

/// The files of a C library in a bundle, relative to it ([`c_library_files`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CLibraryFiles {
    /// The shared library: debug builds link it, `velt dev` loads it.
    pub shared: String,
    /// Windows: the import library for `shared`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_lib: Option<String>,
    /// Linux/macOS: the static library release builds link.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "static")]
    pub static_lib: Option<String>,
}

impl NativeMeta {
    /// Parse `native.json` text.
    pub fn parse(text: &str, what: &str) -> Result<NativeMeta, String> {
        crate::json_file::parse(text, &format!("{META_FILE} in {what}"))
    }

    /// Read `<dir>/native.json` (a bundle built by an older velt, with `native.toml`, is an
    /// error saying to rebuild it).
    pub fn read(dir: &Path) -> Result<NativeMeta, String> {
        let path = dir.join(META_FILE);
        let old = dir.join(LEGACY_META_FILE);
        if !path.is_file() && old.is_file() {
            return Err(crate::json_file::legacy_error(
                &old,
                META_FILE,
                "rebuild the bundle with `velt native build`",
            ));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        NativeMeta::parse(&text, &path.display().to_string())
    }

    /// The text of `native.json`.
    pub fn to_json(&self) -> String {
        crate::json_file::to_text(self)
    }
}

/// How a package's library came to be on this machine (reported by `velt install`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeOrigin {
    /// A published, checksum-verified prebuilt bundle.
    Prebuilt,
    /// Built from the package's C sources here (no bundle for this target, or a path package).
    BuiltFromSource,
}

/// A package's native library, ready to load or link (part of [`crate::graph::GraphPackage`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeLib {
    /// The bundle directory.
    pub dir: PathBuf,
    /// Its metadata.
    pub meta: NativeMeta,
    /// Where it came from.
    pub origin: NativeOrigin,
}

impl NativeLib {
    /// Load a bundle directory; its metadata may only name the bundle's own files.
    pub fn open(dir: &Path, origin: NativeOrigin) -> Result<NativeLib, String> {
        let meta = NativeMeta::read(dir)?;
        let id = (
            meta.package.as_str(),
            meta.version.as_str(),
            meta.target.as_str(),
        );
        let what = format!("`{}`", dir.display());
        bundle::check_meta(&meta, id, &bundle::list_files(dir)?, &what)?;
        Ok(NativeLib {
            dir: dir.to_path_buf(),
            meta,
            origin,
        })
    }
}

/// The message for a package whose libraries are C `sources` without a library for `target`:
/// zig builds them, once the user asks.
pub fn missing_c_target_message(
    name: &str,
    version: &str,
    target: &str,
    published: &BTreeMap<String, String>,
) -> String {
    let list = if published.is_empty() {
        "none".to_string()
    } else {
        published.keys().cloned().collect::<Vec<_>>().join(", ")
    };
    format!(
        "`{name} {version}` has no prebuilt native library for {target} (published: {list}).\n\
         Set {}=1 to build its C sources here with the pinned zig, or ask the package author to \
         publish this target.",
        crate::install::FROM_SOURCE_VAR
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_round_trip() {
        let meta = NativeMeta {
            package: "sqlite".into(),
            version: "0.1.0".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            abi: 0,
            exports: BTreeMap::new(),
            c: BTreeMap::from([(
                "sqlite3".into(),
                c_library_files("sqlite", "sqlite3", "x86_64-unknown-linux-gnu"),
            )]),
        };
        let text = meta.to_json();
        assert!(
            text.contains("\"static\": \"static/libvelt_c_sqlite__sqlite3.a\""),
            "{text}"
        );
        // An older velt requires `exports`.
        assert!(text.contains("\"exports\": {}"), "{text}");
        assert!(text.ends_with("}\n"), "{text}");
        assert_eq!(NativeMeta::parse(&text, "test").unwrap(), meta);
    }

    #[test]
    fn missing_target_lists_published_ones() {
        let published = BTreeMap::from([("x86_64-unknown-linux-gnu".into(), "sha256:1".into())]);
        let m = missing_c_target_message("sqlite", "1.2.0", "aarch64-apple-darwin", &published);
        assert!(m.starts_with("`sqlite 1.2.0` has no prebuilt native library for aarch64-apple-darwin (published: x86_64-unknown-linux-gnu)"), "{m}");
        assert!(m.contains(crate::install::FROM_SOURCE_VAR));
    }
}
