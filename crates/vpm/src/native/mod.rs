//! Native libraries of packages (docs/internals/contracts/native_abi.md): a package with
//! `native` in its manifest ships a Rust crate whose library is published **prebuilt per target** as a
//! *bundle*, so users only need `velt`.
//!
//! A bundle is a directory (exchanged as an [`crate::archive`]):
//!
//! ```text
//! native.json                       # NativeMeta: package, version, target, ABI, exports
//! shared/libvelt_native_<pkg>.so    # .dylib on macOS; <crate>.dll + <crate>.dll.lib on Windows
//! static/<pkg>.o                    # Linux/macOS: one prelinked object (only exports global)
//! ```
//!
//! - [`bundle`]: the files, checksums, packing and verified unpacking.
//! - [`build`]: `cargo` + prelinking into a bundle (package authors; the fallback for users on a
//!   target nobody published).
//! - [`exports`]: the export list and signature records read from a built library.

pub mod build;
pub mod bundle;
pub mod exports;
#[doc(hidden)]
pub mod samples;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The version of the runtime function table this `velt` provides (velt_rt's
/// `NATIVE_ABI_VERSION`); bundles needing more are refused before download.
pub const NATIVE_ABI: u32 = 1;

/// File name of a bundle's metadata (generated JSON, [`crate::json_file`]).
pub const META_FILE: &str = "native.json";
/// The metadata's former (TOML) name, no longer read.
pub const LEGACY_META_FILE: &str = "native.toml";

/// Prefix of the signature records the SDK's `#[export]` emits (`velt_sig_<export>`).
pub const SIG_PREFIX: &str = "velt_sig_";

/// `<pkg>_`: what every export of package `pkg`'s library starts with (`-` becomes `_`).
pub fn export_prefix(package: &str) -> String {
    format!("{}_", package.replace('-', "_"))
}

/// `velt_native_<pkg>`: the name the package's native crate gives its library (`[lib] name`), so
/// its files are `libvelt_native_<pkg>.so`, `.dylib` and `velt_native_<pkg>.dll`.
pub fn library_name(package: &str) -> String {
    format!("velt_native_{}", package.replace('-', "_"))
}

/// The file names of `package`'s shared library on `target` and, on Windows, of its import library:
/// `libvelt_native_<pkg>.so` / `.dylib`, `velt_native_<pkg>.dll` + `velt_native_<pkg>.dll.lib`.
/// On Windows the DLL is copied next to the executable, so these names must be unique per package:
/// `velt native build` and every bundle's metadata are held to them.
pub fn library_files(package: &str, target: &str) -> (String, Option<String>) {
    let name = library_name(package);
    if target.contains("windows") {
        (format!("{name}.dll"), Some(format!("{name}.dll.lib")))
    } else if target.contains("apple") {
        (format!("lib{name}.dylib"), None)
    } else {
        (format!("lib{name}.so"), None)
    }
}

/// `velt_native_init_<pkg>`: the library's start-up function.
pub fn init_symbol(package: &str) -> String {
    format!("velt_native_init_{}", package.replace('-', "_"))
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
    /// The runtime table version the library needs (at most [`NATIVE_ABI`] to be usable).
    pub abi: u32,
    /// The shared library, relative to the bundle (`shared/...`).
    pub shared: String,
    /// Windows: the import library for `shared`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_lib: Option<String>,
    /// Linux/macOS: the prelinked object for static (release) links, relative to the bundle.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "static")]
    pub static_obj: Option<String>,
    /// Every export except the init function: name → signature (`(string,u32)->IoResult<u64>`).
    pub exports: BTreeMap<String, String>,
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
    /// Built from the package's sources with cargo (no bundle for this target, or a path package).
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
        let file = |path: &str| {
            let path = dir.join(path);
            std::fs::read(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))
        };
        bundle::check_exports(&meta, &file, &what)?;
        Ok(NativeLib {
            dir: dir.to_path_buf(),
            meta,
            origin,
        })
    }

    /// The shared library (`dlopen` in `velt dev`, linked by debug builds).
    pub fn shared_lib(&self) -> PathBuf {
        self.dir.join(&self.meta.shared)
    }

    /// Windows: the import library of [`NativeLib::shared_lib`].
    pub fn import_lib(&self) -> Option<PathBuf> {
        self.meta.import_lib.as_ref().map(|p| self.dir.join(p))
    }

    /// Linux/macOS: the prelinked object for release builds.
    pub fn static_obj(&self) -> Option<PathBuf> {
        self.meta.static_obj.as_ref().map(|p| self.dir.join(p))
    }

    /// The file names of `package`'s shared library on `target` and, on Windows, of its import library:
    /// `libvelt_native_<pkg>.so` / `.dylib`, `velt_native_<pkg>.dll` + `velt_native_<pkg>.dll.lib`.
    /// On Windows the DLL is copied next to the executable, so these names must be unique per package:
    /// `velt native build` and every bundle's metadata are held to them.
    pub fn library_files(package: &str, target: &str) -> (String, Option<String>) {
        let name = library_name(package);
        if target.contains("windows") {
            (format!("{name}.dll"), Some(format!("{name}.dll.lib")))
        } else if target.contains("apple") {
            (format!("lib{name}.dylib"), None)
        } else {
            (format!("lib{name}.so"), None)
        }
    }

    /// `velt_native_init_<pkg>`.
    pub fn init_symbol(&self) -> String {
        init_symbol(&self.meta.package)
    }
}

/// The message for a package without a library for `target` when it cannot be built here.
pub fn missing_target_message(
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
         Install Rust (https://rustup.rs) to build it from source, or ask the package author to \
         publish this target."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(export_prefix("pg-lite"), "pg_lite_");
        assert_eq!(init_symbol("pg-lite"), "velt_native_init_pg_lite");
    }

    #[test]
    fn meta_round_trip() {
        let meta = NativeMeta {
            package: "sqlite".into(),
            version: "0.1.0".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            abi: 1,
            shared: "shared/libvelt_native_sqlite.so".into(),
            import_lib: None,
            static_obj: Some("static/sqlite.o".into()),
            exports: BTreeMap::from([("sqlite_open".into(), "(string)->IoResult<u64>".into())]),
        };
        let text = meta.to_json();
        assert!(text.contains("\"static\": \"static/sqlite.o\""), "{text}");
        assert!(text.ends_with("}\n"), "{text}");
        assert_eq!(NativeMeta::parse(&text, "test").unwrap(), meta);
    }

    #[test]
    fn missing_target_lists_published_ones() {
        let published = BTreeMap::from([("x86_64-unknown-linux-gnu".into(), "sha256:1".into())]);
        let m = missing_target_message("sqlite", "1.2.0", "aarch64-apple-darwin", &published);
        assert!(m.starts_with("`sqlite 1.2.0` has no prebuilt native library for aarch64-apple-darwin (published: x86_64-unknown-linux-gnu)"), "{m}");
        assert!(m.contains("https://rustup.rs"));
    }
}
