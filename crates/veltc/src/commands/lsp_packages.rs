//! What `velt lsp`'s loader learns from the package graph: the target that uses a `#name`
//! variant file, and the modules of a package's dependencies for import completion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use velt_lsp::ModuleEntry;
use vpm::PackageGraph;

use velt_loader::{self as loader, LoadOptions};

/// A target whose build uses `file`, when it is a variant of a `#name` of its package that the
/// host's build does not use (`vpm::imports::target_selecting`).
pub(super) fn variant_target(graph: &PackageGraph, file: &Path) -> Option<String> {
    let (_, root, imports) = graph.imports_of(file)?;
    if imports.is_empty() {
        return None;
    }
    let key = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let rel = vpm::relpath::relative(&key(file), &key(root));
    let host = velt_codegen_cl::host_triple();
    vpm::imports::target_selecting(imports, &format!("./{rel}"), &host).map(str::to_string)
}

/// The modules of the dependencies of the package containing `file`, in `graph`.
pub(super) fn list_dependencies(graph: Option<&PackageGraph>, file: &Path) -> Vec<ModuleEntry> {
    let Some(package) = graph.and_then(|g| g.enclosing_package(file)) else {
        return vec![];
    };
    let dir = file.parent().unwrap_or(Path::new(""));
    let mut out = vec![];
    for (name, root) in &package.dependencies {
        out.extend(
            velt_lsp::package_module_entries(name, root)
                .into_iter()
                .filter(|e| {
                    matches!(
                        loader::resolve_spec(&e.spec, dir),
                        Ok(loader::ModuleRef::Package { .. })
                    )
                }),
        );
    }
    out
}

/// How the language server loads the document at `root`.
pub(super) fn load_options<'a>(
    root: &Path,
    overlay: &'a HashMap<PathBuf, String>,
    graph: Option<&'a PackageGraph>,
) -> LoadOptions<'a> {
    LoadOptions {
        std_root: loader::std_root(),
        packages: graph.map(|g| g as &dyn loader::PackageResolver),
        root_source: None,
        overlay: Some(overlay),
        std_root_as_std: true,
        test_harness: false,
        // For the default target, unless the document is a `#name` variant only another
        // target uses (`js.velt`): then for that one. The variants of each `#name` are
        // compared as `velt check` does.
        target: graph.and_then(|g| variant_target(g, root)),
        host_condition: None,
        check_variants: true,
        extra_roots: &[],
        std_ast_cache: crate::build_cache::std_ast_cache(),
        demand_std: loader::demand_std_on(),
        demand_executor: false,
    }
}
