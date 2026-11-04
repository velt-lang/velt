//! `declare c("lib") function` (docs/internals/design/c-ffi.md): where a C declaration may be.
//! Its signature is the only statement of the C function's, so only code with a manifest may
//! make one: the standard library and packages, not a lone file. The design's `native.c` entry
//! and the root's `allowNative` narrow this to listed packages in a later step of #2187 (a
//! package can't be told from a program's own package before that).

use std::collections::HashSet;

use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap};
use velt_sema::hir;
use vpm::PackageGraph;

/// The libraries a declaration may name before the manifest maps libraries per target: the
/// platform's C library.
const KNOWN: &[&str] = &["c"];

/// Check C declaration `f` (with `c`): where it is, and the library it names.
pub(super) fn check(
    f: &hir::ExternFnDef,
    c: &hir::CImport,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    diags: &mut Diagnostics,
) {
    let in_std = std_files.contains(&f.span.file);
    let file = &sm.get(f.span.file).path;
    let in_package = graph.is_some_and(|g| g.package_of(file).is_some());
    if !in_std && !in_package {
        let name = velt_vir::system::base_name(&f.symbol);
        diags.push(
            Diagnostic::error(
                format!(
                    "`declare c function {name}` is only allowed in a package: this file has no package.vlt"
                ),
                f.span,
            )
            .with_note(
                "put the declaration and a safe API around it in a package (a local one in the workspace works), and import the API",
            ),
        );
        return;
    }
    if !KNOWN.contains(&c.library.as_str()) {
        diags.push(
            Diagnostic::error(
                format!(
                    "C library `{}` is not known: only `c`, the platform's C library, can be named yet",
                    c.library
                ),
                f.span,
            )
            .with_note("libraries mapped per target in package.vlt (`native.c`) come in a later step of #2187"),
        );
    }
}
