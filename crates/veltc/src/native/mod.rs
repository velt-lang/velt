//! The compiler's side of native code in packages (docs/internals/design/c-ffi.md):
//!
//! - [`check_declares`]: outside the standard library, a function without a body is a `declare c`
//!   function of a library its own package names (c_decls.rs), checked against its headers
//!   (headers/); a plain `declare function` there would let ordinary code call arbitrary symbols,
//!   so it is a compile error. (Packages' Rust crates and the native ABI they were called
//!   through are gone, velt-lang/velt#2563.)
//! - [`program_links`]: what the linker adds for the C libraries packages build from `sources`.
//! - [`check_trust`]: `unsafe` code and the raw FFI API only in std and allowed native packages,
//!   each piece with a `// SAFETY:` comment; and what `velt check` lists about them.

use std::collections::HashSet;
use std::path::Path;

use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap};
use velt_sema::hir::{self, Def};
use vpm::PackageGraph;

/// Check every `declare function` outside the standard library (`std_files`: the files of std
/// modules): each must be a `declare c` function, whose library (c_decls.rs, in its package of
/// `graph`, `None` for a program without packages) and headers are checked; see the module
/// docs. With `check_std` (std's root), std's own C declarations are checked against the C
/// library's headers too (`velt native headers --std`). Reports errors into `diags`, and
/// returns the C libraries that passed the header check with their records, for
/// [`note_checked`].
#[allow(clippy::too_many_arguments)]
pub fn check_declares(
    p: &hir::Program,
    sm: &SourceMap,
    check_std: Option<&Path>,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    diags: &mut Diagnostics,
) -> CChecked {
    let mut checked = c_decls::Checked::default();
    for def in &p.defs {
        if let Def::Global(g) = def {
            if let Some(c) = &g.c_global {
                c_decls::check_global(g, c, sm, graph, std_files, target, &mut checked, diags);
            }
            continue;
        }
        let Def::ExternFn(f) = def else { continue };
        if f.is_decorator {
            continue;
        }
        // The JS host provides it (sema checked what crosses; lowering, the target).
        if f.host.is_some() {
            continue;
        }
        if let Some(c) = &f.c {
            c_decls::check(f, c, sm, graph, std_files, target, &mut checked, diags);
            continue;
        }
        if std_files.contains(&f.span.file) {
            continue;
        }
        diags.push(not_c(f));
    }
    if let Some(root) = check_std {
        headers::check_std(p, sm, root, std_files, target, diags);
    }
    headers::check(p, sm, graph, std_files, target, diags)
}

/// The error for a `declare function` (not `declare c`) outside the standard library.
fn not_c(f: &hir::ExternFnDef) -> Diagnostic {
    let kind = if f.is_async {
        "declare async function"
    } else {
        "declare function"
    };
    Diagnostic::error(
        format!(
            "`{kind} {}` is not allowed outside the standard library",
            f.symbol
        ),
        f.span,
    )
    .with_note(
        "to call C code, declare the function with `declare c(\"<library>\")` and list the \
         library in `native.c` of package.velt, or use the standard library's API",
    )
}

/// What the linker adds for a program: the C libraries of `c_libs` built from `sources`.
pub fn program_links(
    graph: Option<&PackageGraph>,
    c_libs: &[String],
    release: bool,
    target: &str,
) -> Result<Vec<velt_link::NativeLink>, String> {
    c_sources::links(graph, c_libs, release, target)
}

mod c_decls;
mod c_sources;
mod headers;
pub use c_decls::{link_args as c_link_args, links as c_links};
pub use c_sources::{dev_libraries as c_dev_libraries, system_libraries as c_system_libraries};
pub use headers::{
    read_constants as read_c_constants, stamp_inputs as c_stamp_inputs, zig_target,
    Checked as CChecked, Constants as CConstants,
};
pub use report::note_checked;
mod report;
pub use report::{json as native_json, render as render_native, NativePackage};
mod trust;
mod unsafe_sites;

/// Where `module` first has unsafe code, if it has any: what a generator's output can't have, as
/// the trust check reads only the loaded modules.
pub(crate) fn first_unsafe_code(module: &velt_syntax::ast::Module) -> Option<velt_common::Span> {
    unsafe_sites::find(module).sites.first().map(|s| s.span)
}

/// [`trust`]'s checks of `modules` (errors into `diags`), and the packages with native code the
/// program allows, with their unsafe code, for `velt check` to list.
pub fn check_trust(
    modules: &[velt_sema::SourceModule],
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    target: &str,
    diags: &mut Diagnostics,
) -> Vec<NativePackage> {
    let sites = trust::check(modules, sm, graph, diags);
    report::native_packages(graph, &sites, sm, target)
}

#[cfg(test)]
mod tests;
