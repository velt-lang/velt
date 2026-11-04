//! What `velt lsp`'s loader learns from the package graph: the target that uses a `#name`
//! variant file, and the modules of a package's dependencies for import completion.

use std::path::Path;

use velt_lsp::ModuleEntry;
use vpm::PackageGraph;

use velt_loader as loader;

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
