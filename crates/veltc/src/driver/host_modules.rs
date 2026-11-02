//! A `--host` build's host modules (docs/internals/contracts/workers.md): every import module the
//! program calls (a reachable `declare host("m")` function) needs its JavaScript in the glue the
//! build writes. The runtime glue provides the `velt:` modules; a package provides the others,
//! through `hostModules` in its `package.vlt`. A package provides only modules its own
//! modules declare, so a dependency cannot take over another package's module, and each module
//! comes from one package. Checked where the HIR and the VIR meet: the declarations say who
//! declares a module, the VIR's externs which modules the program reaches.

use std::collections::{BTreeMap, BTreeSet};

use velt_common::{Diagnostic, SourceMap, Span};
use velt_sema::hir;
use velt_vir::vir;
use vpm::graph::GraphPackage;
use vpm::host_modules::RUNTIME_PREFIX;
use vpm::PackageGraph;

/// Diagnostics for the host modules `program` imports that a build for `host` has no JavaScript
/// for, located at a declaration of each.
pub(super) fn check(
    hir: &hir::Program,
    program: &vir::Program,
    sm: &SourceMap,
    packages: Option<&PackageGraph>,
    host: &str,
) -> Vec<Diagnostic> {
    let imported: BTreeSet<&str> = program
        .externs
        .iter()
        .filter_map(|e| e.host.as_ref())
        .map(|h| h.module.as_str())
        .filter(|m| !m.starts_with(RUNTIME_PREFIX))
        .collect();
    // Module → the packages declaring it (by root; `None`: outside every package), with a
    // declaration's location each.
    let mut declared: BTreeMap<&str, Vec<(Option<&GraphPackage>, Span)>> = BTreeMap::new();
    for def in &hir.defs {
        let hir::Def::ExternFn(f) = def else {
            continue;
        };
        let Some(import) = f
            .host
            .as_ref()
            .filter(|h| imported.contains(h.module.as_str()))
        else {
            continue;
        };
        let package = packages.and_then(|g| g.package_of(&sm.get(f.span.file).path));
        let by = declared.entry(import.module.as_str()).or_default();
        if !by.iter().any(|(p, _)| same(*p, package)) {
            by.push((package, f.span));
        }
    }
    let all = packages.map_or(&[][..], PackageGraph::packages);
    let mut out = vec![];
    for (module, declarers) in declared {
        let providers: Vec<&GraphPackage> = all
            .iter()
            .filter(|p| p.host_modules.contains_key(module))
            .collect();
        out.extend(module_diagnostics(module, &declarers, &providers, host));
    }
    out
}

/// Whether two declarations come from the same package.
fn same(a: Option<&GraphPackage>, b: Option<&GraphPackage>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.root == b.root,
        (None, None) => true,
        _ => false,
    }
}

/// The diagnostics for host module `module`, which `declarers` declare (each with a location)
/// and `providers` list under `hostModules`.
fn module_diagnostics(
    module: &str,
    declarers: &[(Option<&GraphPackage>, Span)],
    providers: &[&GraphPackage],
    host: &str,
) -> Vec<Diagnostic> {
    let span = declarers.first().map_or(Span::DUMMY, |(_, s)| *s);
    let mut out = vec![];
    let mut valid = vec![];
    for p in providers {
        if declarers.iter().any(|(d, _)| same(*d, Some(p))) {
            valid.push(p.name.as_str());
        } else {
            out.push(
                Diagnostic::error(
                    format!(
                        "package `{}` provides host module `{module}`, which none of its modules \
                     declares",
                        p.name
                    ),
                    span,
                )
                .with_note(
                    "a package provides only the host modules of its own `declare host` functions",
                ),
            );
        }
    }
    match valid.as_slice() {
        [_] => {}
        [] if out.is_empty() => out.push(missing(module, declarers, span, host)),
        [] => {}
        [a, b, ..] => out.push(Diagnostic::error(
            format!("host module `{module}` is provided by two packages, `{a}` and `{b}`"),
            span,
        )),
    }
    out
}

/// The error for a host module no package provides.
fn missing(
    module: &str,
    declarers: &[(Option<&GraphPackage>, Span)],
    span: Span,
    host: &str,
) -> Diagnostic {
    let d = Diagnostic::error(
        format!("the `{host}` build has no JavaScript for host module `{module}`"),
        span,
    );
    match declarers.iter().find_map(|(p, _)| *p) {
        Some(p) => d.with_note(format!(
            "list it in `{}`'s package.vlt: `hostModules: {{ \"{module}\": \"./src/{module}.mjs\" }}`",
            p.name
        )),
        None => d.with_note(
            "only a package provides host modules (`hostModules` in its package.vlt): make the \
             program a package, or import the module from one",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn graph(packages: &[(&str, &[&str])]) -> PackageGraph {
        let mut g = PackageGraph::default();
        for (name, modules) in packages {
            let p = g.add(name, Path::new(&format!("/p/{name}")), BTreeMap::new());
            for m in *modules {
                p.host_modules
                    .insert(m.to_string(), format!("./src/{m}.mjs"));
            }
        }
        g
    }

    fn messages(g: &PackageGraph, declared_by: &[Option<&str>], module: &str) -> Vec<String> {
        let find = |name: &str| g.packages().iter().find(|p| p.name == name);
        let declarers: Vec<(Option<&GraphPackage>, Span)> = declared_by
            .iter()
            .map(|d| (d.and_then(find), Span::DUMMY))
            .collect();
        let providers: Vec<&GraphPackage> = g
            .packages()
            .iter()
            .filter(|p| p.host_modules.contains_key(module))
            .collect();
        module_diagnostics(module, &declarers, &providers, "workers")
            .iter()
            .map(|d| format!("{} {:?}", d.message, d.notes))
            .collect()
    }

    #[test]
    fn a_module_comes_from_the_package_declaring_it() {
        let g = graph(&[("app", &[]), ("kv", &["kv"])]);
        assert!(messages(&g, &[Some("kv")], "kv").is_empty());
        // Another package declaring it too uses the one the provider ships.
        assert!(messages(&g, &[Some("app"), Some("kv")], "kv").is_empty());
    }

    #[test]
    fn a_module_without_javascript_is_an_error_saying_where_to_add_it() {
        let g = graph(&[("app", &[])]);
        let got = messages(&g, &[Some("app")], "kv");
        assert_eq!(got.len(), 1);
        assert!(
            got[0].contains("the `workers` build has no JavaScript for host module `kv`")
                && got[0].contains("list it in `app`'s package.vlt"),
            "{got:?}"
        );
        let got = messages(&g, &[None], "kv");
        assert!(
            got[0].contains("only a package provides host modules"),
            "{got:?}"
        );
    }

    #[test]
    fn a_package_provides_only_modules_it_declares() {
        let g = graph(&[("app", &[]), ("rogue", &["kv"])]);
        let got = messages(&g, &[Some("app")], "kv");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].contains(
                "package `rogue` provides host module `kv`, which none of its modules declares"
            ),
            "{got:?}"
        );
        // The declaring package's own file still wins over an impostor's.
        let g = graph(&[("kv", &["kv"]), ("rogue", &["kv"])]);
        let got = messages(&g, &[Some("kv")], "kv");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("`rogue`"), "{got:?}");
    }

    #[test]
    fn one_module_comes_from_one_package() {
        let g = graph(&[("a", &["kv"]), ("b", &["kv"])]);
        let got = messages(&g, &[Some("a"), Some("b")], "kv");
        assert_eq!(
            got,
            ["host module `kv` is provided by two packages, `a` and `b` []"]
        );
    }
}
