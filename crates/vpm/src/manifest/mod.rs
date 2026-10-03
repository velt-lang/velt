//! The package manifest, `package.vlt` (CONTRACT: docs/internals/contracts/manifest.md):
//!
//! ```ts ignore
//! import type { Package } from "velt:package";
//!
//! export const pkg: Package = {
//!   name: "hello",
//!   version: "0.1.0",
//!   entry: "src/main.vlt",              // optional, this is the default
//!   registry: "https://registry.example.com",   // optional remote registry
//!   dependencies: {
//!     json: "1.2",                      // registry package, semver requirement
//!     util: { path: "../util" },        // local package
//!   },
//!   paths: { "@app/*": "src/*" },       // import aliases (`crate::paths`)
//!   jsx: { importSource: "sigx" },      // JSX runtime of the package's modules
//!   native: { targets: ["x86_64-unknown-linux-gnu"] },  // a Rust crate (`crate::native`)
//! };
//! ```
//!
//! [`read`] parses it without compiling or running anything. [`legacy`] only turns an old
//! `velt.toml` into the error that tells its owner what to write instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use velt_common::{Diagnostics, FileId, SourceMap};

pub mod ide;
pub mod legacy;
pub mod read;
pub mod schema;
pub(crate) mod write;

/// File name of the manifest at a package root.
pub const MANIFEST_FILE: &str = "package.vlt";
/// The manifest's former name: still found, to report how to migrate it ([`legacy`]).
pub const LEGACY_MANIFEST_FILE: &str = "velt.toml";
/// Default runnable entry, relative to the package root.
pub const DEFAULT_ENTRY: &str = "src/main.vlt";
/// Entry module seen by importers of a library package, relative to the package root.
pub const LIB_ENTRY: &str = "src/lib.vlt";
/// Directory holding a package's modules, relative to the package root.
pub const SRC_DIR: &str = "src";

/// A parsed and validated manifest. The serde attributes describe the legacy `velt.toml` layout
/// ([`legacy`]); `package.vlt` is decoded by [`read`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    /// Top-level `registry = "https://…"`: the remote registry for this package's registry
    /// dependencies and `velt publish` (default: the local registry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// `name`, `version` and `entry` (`[package]` in `velt.toml`).
    pub package: Package,
    /// `dependencies`, keyed by package name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,
    /// `paths`: import specifier pattern → module path relative to the package root
    /// (`"@app/*" = "src/*"`, see [`crate::paths`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paths: BTreeMap<String, String>,
    /// `native`: the package ships a native library built from a Cargo crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeConfig>,
    /// `jsx`: how the package's modules compile JSX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsx: Option<JsxConfig>,
}

/// The `jsx` object (docs/internals/contracts/jsx.md "Choosing the provider").
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsxConfig {
    /// `importSource = "sigx"`: the module whose `jsx-runtime` provides the JSX factories (a
    /// package, a `std/` module, a `paths` alias, or `./dir` relative to the package root);
    /// a `// @jsxImportSource` pragma in a file wins.
    #[serde(
        default,
        rename = "importSource",
        skip_serializing_if = "Option::is_none"
    )]
    pub import_source: Option<String>,
}

/// The `native` object.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeConfig {
    /// The Cargo crate's directory: one directory name inside the package root (default `native`).
    #[serde(default = "default_native_path")]
    pub path: String,
    /// The targets `velt publish` must publish a prebuilt library for.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Whether a `wasm32-wasip1` library is published too (not supported yet: must be false).
    #[serde(default)]
    pub wasm: bool,
}

/// Default `native.path`.
pub const DEFAULT_NATIVE_PATH: &str = "native";

fn default_native_path() -> String {
    DEFAULT_NATIVE_PATH.to_string()
}

/// The targets a native library can be published for (the targets `velt` itself builds for).
pub const NATIVE_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The package's identity: `name`, `version` and `entry`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Package {
    /// Package name, `[a-z][a-z0-9_-]*`.
    pub name: String,
    /// Semver version (validated).
    pub version: String,
    /// One line about the package, for `velt search` and registry listings.
    #[serde(default)]
    pub description: Option<String>,
    /// Search words (`[a-z0-9][a-z0-9-]{0,31}`, at most [`MAX_KEYWORDS`]).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Entry module, relative to the manifest directory.
    #[serde(default = "default_entry")]
    pub entry: String,
}

/// Longest `description`, in characters (Unicode scalar values): one line in `velt search`.
pub const MAX_DESCRIPTION: usize = 300;
/// Most `keywords`.
pub const MAX_KEYWORDS: usize = 10;
/// Longest keyword.
pub const MAX_KEYWORD: usize = 32;

fn default_entry() -> String {
    DEFAULT_ENTRY.to_string()
}

/// One `dependencies` entry.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    /// `name = "1.2"`
    Version(String),
    /// `name = { version = "1.2" }` or `name = { path = "../x" }`
    Detailed(DetailedDependency),
}

/// Table form of a dependency.
#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
pub struct DetailedDependency {
    /// Semver requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Local package directory, relative to the depending manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Dependency {
    /// The semver requirement, if any.
    pub fn version(&self) -> Option<&str> {
        match self {
            Dependency::Version(v) => Some(v),
            Dependency::Detailed(d) => d.version.as_deref(),
        }
    }

    /// The local path, if this is a path dependency.
    pub fn path(&self) -> Option<&str> {
        match self {
            Dependency::Version(_) => None,
            Dependency::Detailed(d) => d.path.as_deref(),
        }
    }
}

impl Manifest {
    /// Read and validate `package.vlt` text; errors are rendered against the name `package.vlt`.
    pub fn parse(src: &str) -> Result<Manifest, String> {
        Manifest::read(FileId(0), src).map_err(|d| render(Path::new(MANIFEST_FILE), src, &d))
    }

    /// Read and validate the manifest file at `path`. The file is never read past
    /// [`read::MAX_BYTES`].
    pub fn from_path(path: &Path) -> Result<Manifest, String> {
        Manifest::from_path_shown_as(path, path)
    }

    /// [`Manifest::from_path`] with every message naming the file `shown` instead of `path`: the
    /// registry server reads uploads from a staging directory its clients must not see.
    pub fn from_path_shown_as(path: &Path, shown: &Path) -> Result<Manifest, String> {
        let cannot = |e: &dyn std::fmt::Display| format!("cannot read {}: {e}", shown.display());
        let file = std::fs::File::open(path).map_err(|e| cannot(&e))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, read::MAX_BYTES as u64 + 1),
            &mut bytes,
        )
        .map_err(|e| cannot(&e))?;
        if bytes.len() > read::MAX_BYTES {
            return Err(format!(
                "{}: the manifest is larger than {} KiB",
                shown.display(),
                read::MAX_BYTES / 1024
            ));
        }
        let src = String::from_utf8(bytes)
            .map_err(|_| format!("{}: the manifest is not valid UTF-8", shown.display()))?;
        Manifest::read(FileId(0), &src).map_err(|d| render(shown, &src, &d))
    }

    /// Read the manifest of the package rooted at `dir`. A directory with only a `velt.toml` gets
    /// the migration error ([`legacy::migration_error`]).
    pub fn from_dir(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join(MANIFEST_FILE);
        if !path.is_file() && dir.join(LEGACY_MANIFEST_FILE).is_file() {
            return Err(legacy::migration_error(dir));
        }
        Manifest::from_path(&path)
    }

    /// The manifest as `package.vlt` text, formatted like `velt fmt` formats it. Defaults are left
    /// out.
    pub fn to_vlt(&self) -> String {
        write::to_vlt(self)
    }

    /// The manifest as JSON in `package.vlt`'s shape (`velt manifest --json`), with defaults
    /// filled in.
    pub fn to_json(&self) -> serde_json::Value {
        write::to_json(self)
    }

    /// The package version as a parsed semver version (validated on parse).
    pub fn version(&self) -> semver::Version {
        semver::Version::parse(&self.package.version)
            .expect("ICE: manifest version validated on parse")
    }
}

/// Diagnostics of the manifest text `src` at `path`, rendered like compiler errors.
fn render(path: &Path, src: &str, diags: &Diagnostics) -> String {
    let mut sm = SourceMap::new();
    sm.add(path, src);
    let parts: Vec<String> = diags.iter().map(|d| d.render(&sm)).collect();
    parts.join("\n\n")
}

// The field checks of [`read`], which attaches a location to their messages.

/// `entry` is a `/`-separated path to a module inside the package.
fn check_entry(entry: &str) -> Result<(), String> {
    // Checked on the text, not with `std::path`, so a manifest means the same on every OS
    // (`C:/x` is a drive on Windows only).
    let segments: Vec<&str> = entry.split('/').collect();
    let inside = !entry.contains(['\\', ':'])
        && segments.iter().all(|s| !s.is_empty() && *s != "..")
        && segments.iter().any(|s| *s != ".");
    if inside {
        Ok(())
    } else {
        Err(format!(
            "entry `{entry}` must be a `/`-separated path to a file inside the package"
        ))
    }
}

fn check_registry(url: &str) -> Result<(), String> {
    if crate::locations::is_url(url) {
        Ok(())
    } else {
        Err(format!(
            "registry `{url}` must be an http:// or https:// URL"
        ))
    }
}

/// `description`: one line of at most [`MAX_DESCRIPTION`] characters, without surrounding
/// whitespace (the stored value is what the file says).
fn check_description(text: &str) -> Result<(), String> {
    let chars = text.chars().count();
    if text.is_empty() {
        Err("`description` is empty; remove the field instead".into())
    } else if chars > MAX_DESCRIPTION {
        Err(format!(
            "`description` has {chars} characters; at most {MAX_DESCRIPTION} are allowed (`velt search` shows the start of a long one)"
        ))
    } else if text.chars().any(char::is_control) {
        Err(
            "`description` must be one line (no line breaks, tabs or other control characters)"
                .into(),
        )
    } else if let Some(c) = text.chars().find(|c| invisible(*c)) {
        Err(format!(
            "`description` contains the invisible character U+{:04X} (it can reorder or hide text where the description is shown)",
            c as u32
        ))
    } else if text.trim() != text {
        Err("`description` starts or ends with whitespace".into())
    } else {
        Ok(())
    }
}

/// Line and paragraph separators, and format characters that reorder or hide text (bidi
/// overrides and isolates, zero-width characters, the byte-order mark).
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x061C | 0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064
        | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
}

/// One keyword: lowercase ASCII letters, digits and `-`, starting with a letter or digit, at most
/// [`MAX_KEYWORD`] characters.
fn check_keyword(word: &str) -> Result<(), String> {
    let mut chars = word.chars();
    let ok = word.len() <= MAX_KEYWORD
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "keyword `{word}` must be lowercase letters, digits and `-`, starting with a letter or digit (at most {MAX_KEYWORD} characters)"
        ))
    }
}

fn check_name(name: &str) -> Result<(), String> {
    if RESERVED_NAMES.contains(&name) {
        return Err(reserved(name));
    }
    if is_windows_device_name(name) {
        return Err(device_name(name));
    }
    if LIBC_PREFIX_NAMES.contains(&name) {
        return Err(libc_prefix(name));
    }
    if is_valid_package_name(name) {
        Ok(())
    } else {
        Err(format!(
            "invalid package name `{name}` (use lowercase letters, digits, `-` and `_`, starting with a letter)"
        ))
    }
}

/// The message names the value, not the field: callers prefix it.
fn check_version(version: &str) -> Result<(), String> {
    semver::Version::parse(version)
        .map(drop)
        .map_err(|e| format!("`{version}` is not a semver version: {e}"))
}

/// The message names the value, not the table: callers prefix it.
fn check_import_source(source: &str) -> Result<(), String> {
    if source.is_empty() || source.ends_with('/') || source.contains('\\') {
        Err(format!("importSource `{source}` is not a module specifier"))
    } else {
        Ok(())
    }
}

fn check_dependency_name(name: &str) -> Result<(), String> {
    if RESERVED_NAMES.contains(&name) {
        return Err(reserved(name));
    }
    if is_windows_device_name(name) {
        return Err(device_name(name));
    }
    if LIBC_PREFIX_NAMES.contains(&name) {
        return Err(libc_prefix(name));
    }
    if is_valid_package_name(name) {
        Ok(())
    } else {
        Err(format!("invalid dependency name `{name}`"))
    }
}

pub(crate) fn check_dependency(name: &str, dep: &Dependency) -> Result<(), String> {
    if dep.version().is_none() && dep.path().is_none() {
        return Err(format!("dependency `{name}` needs a `version` or a `path`"));
    }
    if let Some(req) = dep.version() {
        semver::VersionReq::parse(req).map_err(|e| {
            format!("dependency `{name}`: invalid version requirement `{req}`: {e}")
        })?;
    }
    Ok(())
}

/// The message names the value, not the table: callers prefix it.
fn check_native_path(path: &str) -> Result<(), String> {
    let ok = !path.is_empty()
        && !path.starts_with('.')
        && path != SRC_DIR
        && path != "target"
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "path `{path}` must be the name of a directory in the package root (not `src` or `target`)"
        ))
    }
}

/// The message names the value, not the table: callers prefix it.
fn check_native_target(target: &str) -> Result<(), String> {
    if NATIVE_TARGETS.contains(&target) {
        Ok(())
    } else {
        Err(format!(
            "target `{target}` is not supported (supported: {})",
            NATIVE_TARGETS.join(", ")
        ))
    }
}

/// The message continues the caller's spelling of `wasm = true`, which differs per format.
fn check_native_wasm(wasm: bool) -> Result<(), String> {
    if wasm {
        Err("is not supported yet: packages with native code cannot target WebAssembly".into())
    } else {
        Ok(())
    }
}

/// Whether `name` is a valid package name: `[a-z][a-z0-9_-]*`, not reserved and not a Windows
/// device name (a package is a directory named after it).
pub fn is_valid_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    !RESERVED_NAMES.contains(&name)
        && !is_windows_device_name(name)
        && !LIBC_PREFIX_NAMES.contains(&name)
        && matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
}

/// Names no package may have: a package's modules are named after it (`std/x`), and `std` is the
/// standard library's namespace.
pub const RESERVED_NAMES: &[&str] = &["std"];

/// Whether `name` is a device name on Windows (`con`, `prn`, `aux`, `nul`, `com0`–`com9`,
/// `lpt0`–`lpt9`, in any case): a file or directory of that name can't be created there, so a
/// package or registry user may not have it.
pub fn is_windows_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match lower.as_bytes() {
        b"con" | b"prn" | b"aux" | b"nul" => true,
        [b'c', b'o', b'm', d] | [b'l', b'p', b't', d] => d.is_ascii_digit(),
        _ => false,
    }
}

fn device_name(name: &str) -> String {
    format!("the package name `{name}` is a device name on Windows (`con`, `nul`, `com1`, …)")
}

/// Names whose export prefix (`<name>_`, see `native::export_prefix`) is a C library namespace
/// (`pthread_create`, `sem_open`, `shm_open`, `posix_spawn`): such a package's native exports
/// would share names with the C library's functions.
pub const LIBC_PREFIX_NAMES: &[&str] = &["pthread", "sem", "shm", "posix"];

fn libc_prefix(name: &str) -> String {
    format!(
        "the package name `{name}` is reserved: its native functions (`{name}_*`) would share names with the C library's"
    )
}

fn reserved(name: &str) -> String {
    format!("the package name `{name}` is reserved for the standard library")
}

/// The root directory of the nearest package enclosing `start` (a file or directory): the first
/// ancestor containing a `package.vlt` (or a `velt.toml`, which [`Manifest::from_dir`] then
/// reports as needing migration).
pub fn find_package_root(start: &Path) -> Option<PathBuf> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    start
        .ancestors()
        .find(|d| d.join(MANIFEST_FILE).is_file() || d.join(LEGACY_MANIFEST_FILE).is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests;
