//! `native.c.<library>` of package.vlt (docs/internals/design/c-ffi.md): where a C library
//! comes from per target, and the headers its declarations are checked against.

use super::Manifest;

/// A C library of a package (`native.c.<library>`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CLibraryConfig {
    /// `system`: the platform's library by build condition, in the order written
    /// (`"sqlite3"`, `"framework:Accelerate"`).
    pub system: Vec<(String, String)>,
    /// `headers`: the C headers the library's declarations are checked against, as written: a
    /// path relative to the package root (`native/sqlite3.h`), or `<string.h>` for a header of
    /// the C library's.
    pub headers: Vec<String>,
    /// `unchecked: true`: the library has no headers (a binary-only SDK), so its declarations
    /// go unchecked, which `velt check` and `allowNative`'s error say.
    pub unchecked: bool,
    /// `sources`: the C files of a library the package ships, relative to the package root, each
    /// under `native/`. `velt native build` compiles them with the pinned zig.
    pub sources: Vec<String>,
    /// `defines`: the preprocessor definitions its sources and headers are compiled with.
    pub defines: Vec<(String, String)>,
}

/// The directory of a package holding what its `sources` libraries are built from: all of it
/// is distributed with the package, and a change in it rebuilds them.
pub const C_SOURCES_DIR: &str = "native";

impl Manifest {
    /// Whether the package ships C libraries as `sources`.
    pub fn has_c_sources(&self) -> bool {
        self.c_libraries
            .iter()
            .flatten()
            .any(|(_, lib)| !lib.sources.is_empty())
    }

    /// The package's libraries built from `sources`, by name.
    pub fn c_source_libraries(&self) -> impl Iterator<Item = (&String, &CLibraryConfig)> {
        self.c_libraries
            .iter()
            .flatten()
            .filter(|(_, lib)| !lib.sources.is_empty())
    }

    /// Whether the package publishes prebuilt libraries: it has a Rust crate or C `sources`.
    pub fn has_bundles(&self) -> bool {
        self.native.is_some() || self.has_c_sources()
    }

    /// The targets `velt publish` publishes a prebuilt library for.
    pub fn bundle_targets(&self) -> &[String] {
        match &self.native {
            Some(native) => &native.targets,
            None => &self.c_targets,
        }
    }

    /// Whether the package has native code: a Rust crate, or C declarations (`native.c`). Such a
    /// package is installed only when the program allows it (`allowNative`).
    pub fn has_native_code(&self) -> bool {
        self.native.is_some() || self.c_libraries.is_some()
    }
}

impl CLibraryConfig {
    /// The system library a build for `triple` links: the first `system` entry whose condition
    /// the build has (`None`: the library has none for that target).
    pub fn system_for(&self, triple: &str) -> Option<&str> {
        let conditions = crate::imports::conditions(triple, None);
        self.system
            .iter()
            .find(|(cond, _)| conditions.contains(cond))
            .map(|(_, lib)| lib.as_str())
    }
}
