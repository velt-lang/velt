//! A package's C libraries in its manifest (`native.c`, docs/internals/design/c-ffi.md).

use super::Manifest;

/// A C library of a package (`native.c.<library>`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CLibraryConfig {
    /// `system`: the platform's library by build condition, in the order written
    /// (`"sqlite3"`, `"framework:Accelerate"`).
    pub system: Vec<(String, String)>,
}

impl Manifest {
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
