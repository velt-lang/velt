//! `declare c("lib") function` (docs/internals/design/c-ffi.md): where a C declaration may be,
//! and the library it names. Its signature is the only statement of the C function's, so only
//! code with a manifest may make one: the standard library and packages, not a lone file. `c`
//! is the platform's C library; any other library is one of the package's `native.c` entries,
//! which says what each target links ([`links`]). The root's `allowNative` decides at install
//! which packages with C libraries a program may use (`vpm::install`).

use std::collections::HashSet;

use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap};
use velt_sema::hir;
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
    let in_std = std_files.contains(&f.span.file);
    let file = &sm.get(f.span.file).path;
    let package = graph.and_then(|g| g.package_of(file));
    let name = velt_vir::system::base_name(&f.symbol);
    let Some(package) = package.filter(|_| !in_std) else {
        if !in_std {
            let around = graph.and_then(|g| g.enclosing_package(file));
            let why = match around {
                Some(p) => format!(
                    "this file is in an npm package under `{}`, which isn't part of it",
                    p.name
                ),
                None => "this file has no package.vlt".to_string(),
            };
            diags.push(
                Diagnostic::error(
                    format!("`declare c function {name}` is only allowed in a package: {why}"),
                    f.span,
                )
                .with_note(
                    "put the declaration and a safe API around it in a package (a local one in the workspace works), and import the API",
                ),
            );
        } else if c.library != C_LIBRARY {
            diags.push(Diagnostic::error(
                format!("ICE: the standard library declares `{name}` in C library `{}`; std names only `c`", c.library),
                f.span,
            ));
        }
        return;
    };
    if !package.has_native_code {
        if !checked.refused.insert(package.name.clone()) {
            return;
        }
        diags.push(
            Diagnostic::error(
                format!(
                    "package `{}` declares C function `{name}`, but its package.vlt doesn't say it has native code",
                    package.name
                ),
                f.span,
            )
            .with_note(
                "list its C libraries and their headers in `native.c` (`native: { c: { c: { headers: [\"<string.h>\"] } } }` when it calls only the platform's C library, `c`): programs then see it has native code and allow it with `allowNative`",
            ),
        );
        return;
    }
    if graph.is_some_and(|g| !g.allows_native(package)) {
        if !checked.refused.insert(package.name.clone()) {
            return;
        }
        diags.push(
            Diagnostic::error(
                format!(
                    "package `{}` declares C function `{name}`, and C code runs with no safety net: the program doesn't allow it",
                    package.name
                ),
                f.span,
            )
            .with_note(format!(
                "if you trust it, list it in the program's package.vlt: `allowNative: [\"{}\"]`",
                package.name
            )),
        );
        return;
    }
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
                format!("C library `{}` is not in package.vlt's `native.c`: {listed}", c.library),
                f.span,
            )
            .with_note(format!(
                "say where it comes from on each target: `native: {{ c: {{ {}: {{ system: {{ default: \"{}\" }} }} }} }}`",
                c.library, c.library
            )),
        );
        return;
    };
    if lib.system_for(target).is_none() {
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

/// The system libraries (`sqlite3`, `framework:Accelerate`) of the C libraries `program`'s
/// declarations name, for `target`: each once, in declaration order. The platform's C library is
/// linked anyway.
pub fn links(
    program: &hir::Program,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    target: &str,
) -> Vec<String> {
    let Some(graph) = graph else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    for def in &program.defs {
        let hir::Def::ExternFn(f) = def else { continue };
        let Some(c) = f.c.as_ref().filter(|c| c.library != C_LIBRARY) else {
            continue;
        };
        let file = &sm.get(f.span.file).path;
        let Some(system) = graph
            .package_of(file)
            .and_then(|p| p.c_libraries.get(&c.library))
            .and_then(|lib| lib.system_for(target))
        else {
            continue;
        };
        if !seen.iter().any(|s| s == system) {
            seen.push(system.to_string());
        }
    }
    seen
}

/// The linker arguments of `systems` ([`links`]) for `target`: `-lsqlite3`, `-framework
/// Accelerate`; `sqlite3.lib` with MSVC. A manifest has a framework only under `darwin`: another
/// target's linker would open its name as an input file.
pub fn link_args(systems: &[String], target: &str) -> Vec<String> {
    let msvc = target.contains("-windows-msvc");
    let mut args = Vec::new();
    for system in systems {
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
    use super::link_args;

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
