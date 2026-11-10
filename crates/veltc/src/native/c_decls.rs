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
    if std_files.contains(&f.span.file) {
        if c.library != C_LIBRARY {
            diags.push(Diagnostic::error(
                format!("ICE: the standard library declares `{name}` in C library `{}`; std names only `c`", c.library),
                f.span,
            ));
        }
        return;
    }
    let what = What {
        keyword: "function",
        noun: "function",
    };
    let Some(package) = allowed_package(what, name, f.span, sm, graph, checked, diags) else {
        return;
    };
    if c.library == C_LIBRARY {
        return;
    }
    let Some(lib) = package.c_libraries.get(&c.library) else {
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
                format!("C library `{}` is not in package.velt's `native.c`: {listed}", c.library),
                f.span,
            )
            .with_note(format!(
                "say where it comes from on each target: `native: {{ c: {{ {}: {{ system: {{ default: \"{}\" }} }} }} }}`",
                c.library, c.library
            )),
        );
        return;
    };
    if lib.sources.is_empty() && lib.system_for(target).is_none() {
        diags.push(
            Diagnostic::error(
                format!(
                    "C library `{}` of package `{}` has no `system` entry for target `{target}`, so `{name}` can't be called there",
                    c.library, package.name
                ),
                f.span,
            )
            .with_note("add a condition the target has (`native`, `darwin`, `linux`, `win32` or `default`) to its `system` object"),
        );
    }
}

/// How a kind of C declaration reads in the errors.
#[derive(Clone, Copy)]
struct What {
    /// `declare c <keyword>`.
    keyword: &'static str,
    /// `C <noun>`.
    noun: &'static str,
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
    let What { keyword, noun } = what;
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
                format!("`declare c {keyword} {name}` is only allowed in a package: {why}"),
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
        keyword: "const",
        noun: "constant",
    };
    allowed_package(what, name, at, sm, graph, checked, diags)
}

/// The system libraries (`sqlite3`, `framework:Accelerate`) of the C libraries `program`'s
/// declarations name, for `target`, and those built from `sources` ([`super::c_sources`]): each
/// once, in declaration order. The platform's C library is linked anyway, but a declaration
/// from it may need [`c_library_links`] beside it.
pub fn links(
    program: &hir::Program,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    target: &str,
) -> Vec<String> {
    let mut seen = platform_links(&program.defs, target);
    let Some(graph) = graph else {
        return seen;
    };
    for def in &program.defs {
        let hir::Def::ExternFn(f) = def else { continue };
        let Some(c) = f.c.as_ref().filter(|c| c.library != C_LIBRARY) else {
            continue;
        };
        let file = &sm.get(f.span.file).path;
        let Some(package) = graph.package_of(file) else {
            continue;
        };
        let Some(lib) = package.c_libraries.get(&c.library) else {
            continue;
        };
        let link = if lib.sources.is_empty() {
            let Some(system) = lib.system_for(target) else {
                continue;
            };
            system.to_string()
        } else {
            super::c_sources::entry(&package.name, &c.library)
        };
        if !seen.contains(&link) {
            seen.push(link);
        }
    }
    seen
}

/// [`c_library_links`] when `defs` declare a function of the platform's C library.
fn platform_links(defs: &[hir::Def], target: &str) -> Vec<String> {
    let declares_c = defs.iter().any(|def| {
        matches!(def, hir::Def::ExternFn(f) if f.c.as_ref().is_some_and(|c| c.library == C_LIBRARY))
    });
    let links = if declares_c {
        c_library_links(target)
    } else {
        &[]
    };
    links.iter().map(|l| l.to_string()).collect()
}

/// What a build for `target` links when the program declares a function of the platform's C
/// library (`declare c("c")`). With MSVC, `legacy_stdio_definitions`: since Visual Studio 2015 the
/// UCRT's `<stdio.h>` defines `printf`, `snprintf`, `sprintf`, `fprintf`, `vsnprintf` and the rest
/// of the family inline, so `ucrt.lib` exports none of them, and a declaration that only names
/// the symbol links against the library's out-of-line copies. A static library adds only the
/// functions a program calls, so it costs nothing when unused.
fn c_library_links(target: &str) -> &'static [&'static str] {
    if target.contains("-windows-msvc") {
        &["legacy_stdio_definitions"]
    } else {
        &[]
    }
}

/// The linker arguments of `systems` ([`links`]) for `target`: `-lsqlite3`, `-framework
/// Accelerate`; `sqlite3.lib` with MSVC. A manifest has a framework only under `darwin`: another
/// target's linker would open its name as an input file.
pub fn link_args(systems: &[String], target: &str) -> Vec<String> {
    let msvc = target.contains("-windows-msvc");
    let mut args = Vec::new();
    for system in super::c_sources::system_libraries(systems).iter() {
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
    use super::{link_args, platform_links};
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
    /// Other targets, and programs that declare nothing from `c`, link nothing more.
    #[test]
    fn the_c_library_brings_the_out_of_line_stdio_with_msvc() {
        let link = |defs: &[hir::Def], target: &str| {
            let links = platform_links(defs, target);
            link_args(&links, target)
        };
        let c = [snprintf_from("c")];
        let other_library = [snprintf_from("sqlite3")];
        for msvc in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            assert_eq!(link(&c, msvc), ["legacy_stdio_definitions.lib"]);
            assert!(link(&other_library, msvc).is_empty());
            assert!(link(&[], msvc).is_empty());
        }
        for other in [
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "wasm32-wasip1",
        ] {
            assert!(link(&c, other).is_empty(), "{other}: {:?}", link(&c, other));
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
