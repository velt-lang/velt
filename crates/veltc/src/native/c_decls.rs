//! `declare c("lib") function` (docs/internals/design/c-ffi.md): where a C declaration may be,
//! and the library it names. Its signature is the only statement of the C function's, so only
//! code with a manifest may make one: the standard library and packages, not a lone file. `c`
//! is the platform's C library; any other library is one of the package's `native.c` entries,
//! which says what each target links ([`links`]). The root's `allowNative` decides at install
//! which packages with C libraries a program may use (`vpm::install`).

use std::collections::HashSet;

use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap, Span};
use velt_sema::hir;
use vpm::graph::GraphPackage;
use vpm::PackageGraph;

/// The platform's C library: every target links it, and it needs no `native.c` entry.
const C_LIBRARY: &str = "c";

/// What [`links`] holds when the program declares a function of [`C_LIBRARY`]: no library to
/// load (`velt dev` skips it), but the link adds [`c_library_links`] for it.
pub(super) const DECLARES_C: &str = "platform:c";

/// What checking a program's C declarations remembers between them.
#[derive(Default)]
pub(super) struct Checked {
    /// The packages already reported as not allowed (one error each, at its first declaration).
    refused: HashSet<String>,
}

/// Check C declaration `f` (with `c`): where it is, that the program allows its package's
/// native code, and the library it names, which a build for `target` must link.
#[allow(clippy::too_many_arguments)]
pub(super) fn check(
    f: &hir::ExternFnDef,
    c: &hir::CImport,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    checked: &mut Checked,
    diags: &mut Diagnostics,
) {
    let name = velt_vir::system::base_name(&f.symbol);
    let what = What {
        keyword: " function",
        noun: "function",
        verb: "called",
    };
    let at = (std_files, target);
    check_in_library(
        (name, &c.library, f.span),
        what,
        sm,
        graph,
        at,
        checked,
        diags,
    );
}

/// Check C variable `g` (`declare c("lib") let`, with `c`) as [`check`] does a C function.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_global(
    g: &hir::GlobalDef,
    c: &hir::CGlobal,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    checked: &mut Checked,
    diags: &mut Diagnostics,
) {
    let what = What {
        keyword: "(…) let",
        noun: "variable",
        verb: "used",
    };
    let at = (std_files, target);
    check_in_library(
        (&c.symbol, &c.library, g.span),
        what,
        sm,
        graph,
        at,
        checked,
        diags,
    );
}

/// Check C declaration `name` of C library `library` at `span` (a `what`): where it is, that
/// the program allows its package's native code, and the library, which a build for `target`
/// must link.
fn check_in_library(
    (name, library, span): (&str, &str, Span),
    what: What,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    (std_files, target): (&HashSet<FileId>, &str),
    checked: &mut Checked,
    diags: &mut Diagnostics,
) {
    if std_files.contains(&span.file) {
        if library != C_LIBRARY {
            diags.push(Diagnostic::error(
                format!("ICE: the standard library declares `{name}` in C library `{library}`; std names only `c`"),
                span,
            ));
        }
        return;
    }
    let Some(package) = allowed_package(what, name, span, sm, graph, checked, diags) else {
        return;
    };
    if library == C_LIBRARY {
        return;
    }
    let Some(lib) = package.c_libraries.get(library) else {
        let known: Vec<String> = package
            .c_libraries
            .keys()
            .map(|k| format!("`{k}`"))
            .collect();
        let listed = if known.is_empty() {
            format!("package `{}` lists no C libraries", package.name)
        } else {
            format!("package `{}` lists {}", package.name, known.join(", "))
        };
        diags.push(
            Diagnostic::error(
                format!("C library `{}` is not in package.velt's `native.c`: {listed}", library),
                span,
            )
            .with_note(format!(
                "say where it comes from on each target: `native: {{ c: {{ {}: {{ system: {{ default: \"{}\" }} }} }} }}`",
                library, library
            )),
        );
        return;
    };
    if lib.sources.is_empty() && lib.system_for(target).is_none() {
        diags.push(
            Diagnostic::error(
                format!(
                    "C library `{library}` of package `{}` has no `system` entry for target `{target}`, so `{name}` can't be {} there",
                    package.name, what.verb
                ),
                span,
            )
            .with_note("add a condition the target has (`native`, `darwin`, `linux`, `win32` or `default`) to its `system` object"),
        );
    }
}

/// How a kind of C declaration reads in the errors.
#[derive(Clone, Copy)]
struct What {
    /// `declare c<keyword>` (a space first, but before `(…)`).
    keyword: &'static str,
    /// `C <noun>`.
    noun: &'static str,
    /// What a use of one does: `called`.
    verb: &'static str,
}

/// The package of the C declaration `name` at `at` (outside std), when it may be there: in a
/// package with native code that the program allows.
fn allowed_package<'g>(
    what: What,
    name: &str,
    at: Span,
    sm: &SourceMap,
    graph: Option<&'g PackageGraph>,
    checked: &mut Checked,
    diags: &mut Diagnostics,
) -> Option<&'g GraphPackage> {
    let What { keyword, noun, .. } = what;
    let file = &sm.get(at.file).path;
    let Some(package) = graph.and_then(|g| g.package_of(file)) else {
        let around = graph.and_then(|g| g.enclosing_package(file));
        let why = match around {
            Some(p) => format!(
                "this file is in an npm package under `{}`, which isn't part of it",
                p.name
            ),
            None => "this file has no package.velt".to_string(),
        };
        diags.push(
            Diagnostic::error(
                format!("`declare c{keyword} {name}` is only allowed in a package: {why}"),
                at,
            )
            .with_note(
                "put the declaration and a safe API around it in a package (a local one in the workspace works), and import the API",
            ),
        );
        return None;
    };
    if !package.has_native_code {
        if !checked.refused.insert(package.name.clone()) {
            return None;
        }
        diags.push(
            Diagnostic::error(
                format!(
                    "package `{}` declares C {noun} `{name}`, but its package.velt doesn't say it has native code",
                    package.name
                ),
                at,
            )
            .with_note(
                "list its C libraries and their headers in `native.c` (`native: { c: { c: { headers: [\"<string.h>\"] } } }` when it calls only the platform's C library, `c`): programs then see it has native code and allow it with `allowNative`",
            ),
        );
        return None;
    }
    if graph.is_some_and(|g| !g.allows_native(package)) {
        if !checked.refused.insert(package.name.clone()) {
            return None;
        }
        diags.push(
            Diagnostic::error(
                format!(
                    "package `{}` declares C {noun} `{name}`, and C code runs with no safety net: the program doesn't allow it",
                    package.name
                ),
                at,
            )
            .with_note(format!(
                "if you trust it, list it in the program's package.velt: `allowNative: [\"{}\"]`",
                package.name
            )),
        );
        return None;
    }
    Some(package)
}

/// Check that C constant `name` at `at` may be there; its package, when so. std has no manifest
/// to name headers, so it declares none yet (its system numbers stay in Velt until #2187 step 2).
pub(super) fn check_const<'g>(
    name: &str,
    at: Span,
    sm: &SourceMap,
    graph: Option<&'g PackageGraph>,
    std_files: &HashSet<FileId>,
    checked: &mut Checked,
    diags: &mut Diagnostics,
) -> Option<&'g GraphPackage> {
    if std_files.contains(&at.file) {
        diags.push(Diagnostic::error(
            format!("the standard library can't declare C constant `{name}`: it has no manifest naming headers"),
            at,
        ));
        return None;
    }
    let what = What {
        keyword: " const",
        noun: "constant",
        verb: "read",
    };
    allowed_package(what, name, at, sm, graph, checked, diags)
}

/// The system libraries (`sqlite3`, `framework:Accelerate`) of the C libraries `program`'s
/// declarations name, for `target`, and those built from `sources` ([`super::c_sources`]): each
/// once, in declaration order. The platform's C library is linked anyway; a declaration from it
/// adds [`DECLARES_C`] first, on every target (the build's stamp keeps it), for the link.
pub fn links(
    program: &hir::Program,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    target: &str,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    if declares_c(&program.defs) {
        seen.push(DECLARES_C.to_string());
    }
    let Some(graph) = graph else {
        return seen;
    };
    for (library, span) in program.defs.iter().filter_map(c_library_of) {
        if library == C_LIBRARY {
            continue;
        }
        let file = &sm.get(span.file).path;
        let Some(package) = graph.package_of(file) else {
            continue;
        };
        let Some(lib) = package.c_libraries.get(library) else {
            continue;
        };
        let link = if lib.sources.is_empty() {
            let Some(system) = lib.system_for(target) else {
                continue;
            };
            system.to_string()
        } else {
            super::c_sources::entry(&package.name, library)
        };
        if !seen.contains(&link) {
            seen.push(link);
        }
    }
    seen
}

/// The C library `def` declares a function or a variable of, and where.
fn c_library_of(def: &hir::Def) -> Option<(&str, Span)> {
    match def {
        hir::Def::ExternFn(f) => f.c.as_ref().map(|c| (c.library.as_str(), f.span)),
        hir::Def::Global(g) => g.c_global.as_ref().map(|c| (c.library.as_str(), g.span)),
        _ => None,
    }
}

/// Whether `defs` declare a function or a variable of the platform's C library.
fn declares_c(defs: &[hir::Def]) -> bool {
    (defs.iter().filter_map(c_library_of)).any(|(library, _)| library == C_LIBRARY)
}

/// What a build for `target` links when the program declares a function of the platform's C
/// library (`declare c("c")`). With MSVC, `legacy_stdio_definitions`: since Visual Studio 2015 the
/// UCRT's `<stdio.h>` defines `printf`, `snprintf`, `sprintf`, `fprintf`, `vsnprintf` and the rest
/// of the family inline, so `ucrt.lib` exports none of them, and a declaration that only names
/// the symbol links against the library's out-of-line copies. A static library adds only the
/// functions a program calls, so it costs nothing when unused. Only the link has it: `velt dev`
/// finds such a function in no DLL yet.
fn c_library_links(target: &str) -> &'static [&'static str] {
    if target.contains("-windows-msvc") {
        &["legacy_stdio_definitions"]
    } else {
        &[]
    }
}

/// The linker arguments of `systems` ([`links`]) for `target`: `-lsqlite3`, `-framework
/// Accelerate`; `sqlite3.lib` with MSVC. A manifest has a framework only under `darwin`: another
/// target's linker would open its name as an input file. [`DECLARES_C`] adds
/// [`c_library_links`].
pub fn link_args(systems: &[String], target: &str) -> Vec<String> {
    let msvc = target.contains("-windows-msvc");
    let mut args = Vec::new();
    let platform = (systems.iter().any(|s| s == DECLARES_C))
        .then(|| c_library_links(target).iter().map(|l| l.to_string()))
        .into_iter()
        .flatten();
    for system in platform.chain(super::c_sources::system_libraries(systems)) {
        match system.strip_prefix("framework:") {
            Some(framework) if !target.contains("-apple-") => {
                panic!("ICE: framework `{framework}` reached the link for {target}")
            }
            Some(framework) => args.extend(["-framework".to_string(), framework.to_string()]),
            None if msvc => args.push(format!("{system}.lib")),
            None => args.push(format!("-l{system}")),
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::{declares_c, link_args, DECLARES_C};
    use crate::native::c_system_libraries;
    use velt_common::Span;
    use velt_sema::hir;

    fn args(systems: &[&str], target: &str) -> Vec<String> {
        let systems: Vec<String> = systems.iter().map(|s| s.to_string()).collect();
        link_args(&systems, target)
    }

    #[test]
    fn link_args_per_target() {
        let mac = args(&["z", "framework:Accelerate"], "aarch64-apple-darwin");
        assert_eq!(mac, ["-lz", "-framework", "Accelerate"]);
        assert_eq!(args(&["z"], "x86_64-unknown-linux-gnu"), ["-lz"]);
        assert_eq!(args(&["z"], "x86_64-pc-windows-msvc"), ["z.lib"]);
    }

    /// `declare c("<library>") function snprintf(…)`.
    fn snprintf_from(library: &str) -> hir::Def {
        hir::Def::ExternFn(hir::ExternFnDef {
            name: "snprintf".into(),
            symbol: "snprintf".into(),
            params: Vec::new(),
            ret: hir::TyId(0),
            is_async: false,
            system: None,
            host: None,
            c: Some(hir::CImport {
                library: library.into(),
                errno: false,
                params: Vec::new(),
                ret: hir::CTy::Int(hir::IntTy::I32),
                variadic: true,
            }),
            is_decorator: false,
            span: Span::DUMMY,
        })
    }

    /// A `declare c("c")` binding of `snprintf` links on Windows: the UCRT defines the stdio
    /// family inline in `<stdio.h>`, so MSVC's link needs `legacy_stdio_definitions.lib`.
    /// Other targets, and programs that declare nothing from `c`, link nothing more; `velt dev`
    /// loads no library for it (a static library, no DLL).
    #[test]
    fn the_c_library_brings_the_out_of_line_stdio_with_msvc() {
        let links = |defs: &[hir::Def]| -> Vec<String> {
            (declares_c(defs).then(|| DECLARES_C.to_string()).into_iter()).collect()
        };
        let c = links(&[snprintf_from("c")]);
        let other_library = links(&[snprintf_from("sqlite3")]);
        assert_eq!(c, [DECLARES_C]);
        assert!(other_library.is_empty() && links(&[]).is_empty());
        assert!(c_system_libraries(&c).is_empty(), "velt dev would load it");
        for msvc in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            assert_eq!(link_args(&c, msvc), ["legacy_stdio_definitions.lib"]);
            let with_z = [c[0].clone(), "z".to_string()];
            assert_eq!(
                link_args(&with_z, msvc),
                ["legacy_stdio_definitions.lib", "z.lib"]
            );
        }
        for other in [
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "wasm32-wasip1",
        ] {
            assert!(link_args(&c, other).is_empty(), "{other}");
        }
    }

    #[test]
    #[should_panic(expected = "ICE: framework `Accelerate`")]
    fn a_framework_never_reaches_a_windows_link() {
        args(&["framework:Accelerate"], "x86_64-pc-windows-msvc");
    }

    #[test]
    #[should_panic(expected = "ICE: framework `Accelerate`")]
    fn a_framework_never_reaches_a_linux_link() {
        args(&["framework:Accelerate"], "x86_64-unknown-linux-gnu");
    }
}
