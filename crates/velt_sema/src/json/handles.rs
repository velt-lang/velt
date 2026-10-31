//! Which types keep handles in their private fields, which JSON must never carry or forge
//! (docs/std/encoding/json.md, "Private fields"): std's, and those of a package with a native
//! library.

use velt_syntax::ast::ItemKind;

use crate::ctx::Ctx;
use crate::hir::DefId;

/// The packages with a native library among a program's ([`Ctx::native_libraries`]).
#[derive(Default)]
pub(crate) struct NativeLibraries {
    /// The program's own modules (neither std nor a dependency) are a package with one.
    program: bool,
    /// The dependencies with one, by name (the first part of their modules' paths).
    dependencies: Vec<String>,
}

impl Ctx<'_> {
    /// Does type definition `d` keep runtime or native handles in its private fields? A std
    /// type does, and so does every type of a package with a native library (its database
    /// handles are `private handle: u64`, in whichever of its files): see
    /// [`Self::in_native_library`].
    pub(crate) fn keeps_handles(&self, d: DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| self.scopes[a.module].is_std || self.in_native_library(a.module))
    }

    /// Is module `m` part of a package with a native library? A package has one when one of
    /// its modules declares a function of it: outside std, a `declare function` may only name
    /// an export of the package's own native library (the driver checks that), so it marks the
    /// package. A `declare system function` (the C library's) does not.
    fn in_native_library(&self, m: usize) -> bool {
        let libs = self.native_libraries.get_or_init(|| {
            let mut libs = NativeLibraries::default();
            for module in self.modules.iter().filter(|m| !m.is_std) {
                let declares = module.ast.items.iter().any(
                    |i| matches!(&i.kind, ItemKind::ExternFn(s) if !s.is_system && !s.is_intrinsic),
                );
                match (declares, module.is_dependency) {
                    (false, _) => {}
                    (true, false) => libs.program = true,
                    (true, true) => {
                        let name = package_of(&module.path).to_string();
                        if !libs.dependencies.contains(&name) {
                            libs.dependencies.push(name);
                        }
                    }
                }
            }
            libs
        });
        let module = &self.modules[m];
        match (module.is_std, module.is_dependency) {
            (true, _) => false,
            (false, false) => libs.program,
            (false, true) => libs
                .dependencies
                .iter()
                .any(|n| n == package_of(&module.path)),
        }
    }
}

/// The package a dependency's module belongs to, from its canonical path (`sqlite/database`,
/// `@scope/name/sub`).
fn package_of(path: &str) -> &str {
    let parts = if path.starts_with('@') { 2 } else { 1 };
    match path.match_indices('/').nth(parts - 1) {
        Some((i, _)) => &path[..i],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn package_of_a_dependency_module() {
        assert_eq!(super::package_of("sqlite"), "sqlite");
        assert_eq!(super::package_of("sqlite/engine/database"), "sqlite");
        assert_eq!(super::package_of("@acme/db"), "@acme/db");
        assert_eq!(super::package_of("@acme/db/pool"), "@acme/db");
    }
}
