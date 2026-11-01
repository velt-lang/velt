//! The modules the driver hands to sema ([`SourceModule`]).

use velt_common::FileId;
use velt_syntax::ast;

/// One parsed module handed to sema by the driver.
pub struct SourceModule {
    /// Canonical module path: `"main"` for the root file, `"std/fs"`, `"./util"` resolved to a
    /// root-relative path like `"util"`, package modules as `"pkgname"` / `"pkgname/sub"`.
    pub path: String,
    /// Loaded from the standard library root (the loader's `Origin::Std`). Only std modules may
    /// use compiler intrinsics and the private members of std types; never derived from `path`,
    /// which user files must not be able to imitate.
    pub is_std: bool,
    /// Loaded from a package the program depends on (the loader's `Origin::Package`). Like std,
    /// a library to the program: the analyses that report a problem at the program's call into
    /// library code (a callback a library runs on other threads) treat both alike. It grants no
    /// privilege.
    pub is_dependency: bool,
    pub file: FileId,
    pub ast: ast::Module,
    /// For each `import ... from "<spec>"` in this module: spec string → canonical path of the
    /// module it resolved to (the driver already loaded it into `modules`).
    pub imports: Vec<(String, String)>,
    /// Canonical path of the JSX runtime (`<jsxImportSource>/jsx-runtime`, see
    /// docs/internals/contracts/jsx.md) the driver loaded because this module contains JSX;
    /// `None` when it has none. Sema binds it as the namespace `JSX` and lowers JSX to calls of
    /// its exports.
    pub jsx_runtime: Option<String>,
}
