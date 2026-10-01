//! `velt.toml` manifest model (CONTRACT: docs/internals/contracts/velt_toml.md).
//!
//! ```toml
//! registry = "https://registry.example.com"   # optional remote registry
//!
//! [package]
//! name = "hello"
//! version = "0.1.0"
//! entry = "src/main.vlt"   # optional, this is the default
//!
//! [dependencies]
//! json = "1.2"                       # registry package, semver requirement
//! util = { path = "../util" }        # local package
//! http = { version = "0.3" }         # table form of a version requirement
//!
//! [paths]
//! "@app/*" = "src/*"                 # import aliases (`crate::paths`)
//!
//! [jsx]
//! importSource = "sigx"              # JSX runtime of the package's modules (default `velt:jsx`)
//!
//! [native]                           # a Rust crate built into the package's native library
//! path = "native"                    # (`crate::native`; docs/internals/contracts/native_abi.md)
//! targets = ["x86_64-unknown-linux-gnu"]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the manifest at a package root.
pub const MANIFEST_FILE: &str = "velt.toml";
/// Default runnable entry, relative to the package root.
pub const DEFAULT_ENTRY: &str = "src/main.vlt";
/// Entry module seen by importers of a library package, relative to the package root.
pub const LIB_ENTRY: &str = "src/lib.vlt";
/// Directory holding a package's modules, relative to the package root.
pub const SRC_DIR: &str = "src";

/// A parsed and validated `velt.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Top-level `registry = "https://…"`: the remote registry for this package's registry
    /// dependencies and `velt publish` (default: the local registry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// `[package]` table.
    pub package: Package,
    /// `[dependencies]` table, keyed by package name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,
    /// `[paths]` table: import specifier pattern → module path relative to the package root
    /// (`"@app/*" = "src/*"`, see [`crate::paths`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paths: BTreeMap<String, String>,
    /// `[native]` table: the package ships a native library built from a Cargo crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeConfig>,
    /// `[jsx]` table: how the package's modules compile JSX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsx: Option<JsxConfig>,
}

/// The `[jsx]` table (docs/internals/contracts/jsx.md "Choosing the provider").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsxConfig {
    /// `importSource = "sigx"`: the module whose `jsx-runtime` provides the JSX factories (a
    /// package, a `std/` module, a `[paths]` alias, or `./dir` relative to the package root);
    /// a `// @jsxImportSource` pragma in a file wins.
    #[serde(
        default,
        rename = "importSource",
        skip_serializing_if = "Option::is_none"
    )]
    pub import_source: Option<String>,
}

/// The `[native]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// Default `[native] path`.
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

/// The `[package]` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Package name, `[a-z][a-z0-9_-]*`.
    pub name: String,
    /// Semver version (validated).
    pub version: String,
    /// Entry module, relative to the manifest directory.
    #[serde(default = "default_entry")]
    pub entry: String,
}

fn default_entry() -> String {
    DEFAULT_ENTRY.to_string()
}

/// One `[dependencies]` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    /// `name = "1.2"`
    Version(String),
    /// `name = { version = "1.2" }` or `name = { path = "../x" }`
    Detailed(DetailedDependency),
}

/// Table form of a dependency.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    /// Parse and validate manifest text.
    pub fn parse(src: &str) -> Result<Manifest, String> {
        let m: Manifest =
            toml::from_str(src).map_err(|e| format!("invalid {MANIFEST_FILE}: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Read, parse and validate the manifest file at `path`.
    pub fn from_path(path: &Path) -> Result<Manifest, String> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Manifest::parse(&src).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Read the manifest of the package rooted at `dir`.
    pub fn from_dir(dir: &Path) -> Result<Manifest, String> {
        Manifest::from_path(&dir.join(MANIFEST_FILE))
    }

    /// Serialize (loses formatting; use [`crate::edit`] to modify a user's file).
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("ICE: manifest serialization cannot fail")
    }

    /// The package version as a parsed semver version (validated on parse).
    pub fn version(&self) -> semver::Version {
        semver::Version::parse(&self.package.version)
            .expect("ICE: manifest version validated on parse")
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(url) = &self.registry {
            if !crate::locations::is_url(url) {
                return Err(format!(
                    "registry `{url}` must be an http:// or https:// URL"
                ));
            }
        }
        if !is_valid_package_name(&self.package.name) {
            return Err(format!(
                "invalid package name `{}` (use lowercase letters, digits, `-` and `_`, starting with a letter)",
                self.package.name
            ));
        }
        semver::Version::parse(&self.package.version).map_err(|e| {
            format!(
                "package.version `{}` is not a semver version: {e}",
                self.package.version
            )
        })?;
        crate::paths::validate(&self.paths)?;
        if let Some(native) = &self.native {
            validate_native(native)?;
        }
        if let Some(source) = self.jsx.as_ref().and_then(|j| j.import_source.as_deref()) {
            if source.is_empty() || source.ends_with('/') || source.contains('\\') {
                return Err(format!(
                    "[jsx] importSource `{source}` is not a module specifier"
                ));
            }
        }
        for (name, dep) in &self.dependencies {
            if !is_valid_package_name(name) {
                return Err(format!("invalid dependency name `{name}`"));
            }
            if dep.version().is_none() && dep.path().is_none() {
                return Err(format!("dependency `{name}` needs a `version` or a `path`"));
            }
            if let Some(req) = dep.version() {
                semver::VersionReq::parse(req).map_err(|e| {
                    format!("dependency `{name}`: invalid version requirement `{req}`: {e}")
                })?;
            }
        }
        Ok(())
    }
}

fn validate_native(native: &NativeConfig) -> Result<(), String> {
    let p = &native.path;
    let ok = !p.is_empty()
        && !p.starts_with('.')
        && p != SRC_DIR
        && p != "target"
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok {
        return Err(format!(
            "[native] path `{p}` must be the name of a directory in the package root (not `src` or `target`)"
        ));
    }
    for t in &native.targets {
        if !NATIVE_TARGETS.contains(&t.as_str()) {
            return Err(format!(
                "[native] target `{t}` is not supported (supported: {})",
                NATIVE_TARGETS.join(", ")
            ));
        }
    }
    if native.wasm {
        return Err(
            "[native] wasm = true is not supported yet: packages with native code cannot target WebAssembly"
                .into(),
        );
    }
    Ok(())
}

/// Whether `name` is a valid package name: `[a-z][a-z0-9_-]*`.
pub fn is_valid_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
}

/// The root directory of the nearest package enclosing `start` (a file or directory): the first
/// ancestor containing a `velt.toml`.
pub fn find_package_root(start: &Path) -> Option<PathBuf> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    start
        .ancestors()
        .find(|d| d.join(MANIFEST_FILE).is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_manifest() {
        let m = Manifest::parse(
            r#"
            [package]
            name = "hello"
            version = "0.1.0"

            [dependencies]
            json = "1.2"
            util = { path = "../util" }
            http = { version = "0.3" }
            "#,
        )
        .unwrap();
        assert_eq!(m.package.name, "hello");
        assert_eq!(m.package.entry, "src/main.vlt");
        assert_eq!(m.dependencies["json"], Dependency::Version("1.2".into()));
        assert_eq!(m.dependencies["util"].path(), Some("../util"));
        assert_eq!(m.dependencies["http"].version(), Some("0.3"));
        // Round trip.
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
    }

    #[test]
    fn path_aliases() {
        let head = "[package]\nname = \"app\"\nversion = \"1.0.0\"\n[paths]\n";
        let m = Manifest::parse(&format!(
            "{head}\"@app/*\" = \"src/*\"\n\"@cfg\" = \"src/config\"\n"
        ))
        .unwrap();
        assert_eq!(m.paths["@app/*"], "src/*");
        assert_eq!(m.paths["@cfg"], "src/config");
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
        let e = Manifest::parse(&format!("{head}\"@app/*\" = \"../x/*\"\n")).unwrap_err();
        assert!(e.contains("`@app/*`"), "{e}");
    }

    #[test]
    fn jsx_import_source() {
        let head = "[package]\nname = \"app\"\nversion = \"1.0.0\"\n";
        let m = Manifest::parse(&format!("{head}[jsx]\nimportSource = \"sigx\"\n")).unwrap();
        assert_eq!(
            m.jsx.as_ref().and_then(|j| j.import_source.as_deref()),
            Some("sigx")
        );
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
        assert_eq!(Manifest::parse(head).unwrap().jsx, None);
        let e = Manifest::parse(&format!("{head}[jsx]\nimportSource = \"\"\n")).unwrap_err();
        assert!(e.contains("importSource"), "{e}");
        let e = Manifest::parse(&format!("{head}[jsx]\nimport_source = \"x\"\n")).unwrap_err();
        assert!(e.contains("import_source"), "{e}");
    }

    #[test]
    fn custom_entry_and_no_deps() {
        let m = Manifest::parse(
            "[package]\nname = \"app\"\nversion = \"1.0.0\"\nentry = \"main.vlt\"\n",
        )
        .unwrap();
        assert_eq!(m.package.entry, "main.vlt");
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn rejects_bad_manifests() {
        assert!(Manifest::parse("[package]\nname = \"x\"\n")
            .unwrap_err()
            .contains("version"));
        assert!(Manifest::parse("[package]\nname = \"Bad Name\"\nversion = \"1.0.0\"\n").is_err());
        assert!(
            Manifest::parse("[package]\nname = \"x\"\nversion = \"1\"\n")
                .unwrap_err()
                .contains("semver")
        );
        let e = Manifest::parse(
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n[dependencies]\ny = {}\n",
        )
        .unwrap_err();
        assert!(e.contains("`y`"), "{e}");
        let e = Manifest::parse(
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n[dependencies]\ny = \"one\"\n",
        )
        .unwrap_err();
        assert!(e.contains("requirement"), "{e}");
    }

    #[test]
    fn native_table() {
        let head = "[package]\nname = \"db-x\"\nversion = \"1.0.0\"\n[native]\n";
        let m =
            Manifest::parse(&format!("{head}targets = [\"x86_64-unknown-linux-gnu\"]\n")).unwrap();
        let native = m.native.as_ref().unwrap();
        assert_eq!(native.path, "native");
        assert_eq!(native.targets, ["x86_64-unknown-linux-gnu"]);
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
        for (bad, msg) in [
            ("targets = [\"sparc-sun-solaris\"]", "not supported"),
            ("path = \"../x\"", "directory in the package root"),
            ("path = \"src\"", "directory in the package root"),
            ("wasm = true", "WebAssembly"),
            ("crate = \"x\"", "unknown field"),
        ] {
            let e = Manifest::parse(&format!("{head}{bad}\n")).unwrap_err();
            assert!(e.contains(msg), "{bad}: {e}");
        }
    }

    #[test]
    fn finds_enclosing_package() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/src/deep")).unwrap();
        std::fs::write(tmp.path().join("a/velt.toml"), "").unwrap();
        assert_eq!(
            find_package_root(&tmp.path().join("a/src/deep")),
            Some(tmp.path().join("a"))
        );
        assert_eq!(
            find_package_root(&tmp.path().join("a/src/deep/x.vlt")),
            Some(tmp.path().join("a"))
        );
    }
}
