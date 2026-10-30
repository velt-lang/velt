//! The JS API the standard library provides (`body::expr::numbers::is_js_api`): user code sees
//! its integer results as `number`, like TypeScript.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;

/// The prelude modules that are Velt's own API, JS having no counterpart: the ordering protocol
/// (`compareTo`, classes.md) and the integer bit counts. Their integer results stay integers in
/// user code (`x.trailingZeros()` indexes an array as it is). A Velt-only prelude method goes in
/// one of them, or in a new module listed here.
const VELT_PRELUDE: &[&str] = &["std/prelude/compare", "std/prelude/bits"];

/// The modules of Velt-only globals (no JavaScript counterpart): like `VELT_PRELUDE`, their
/// integer parameters and results stay integers (`StringBuilder.appendInt` takes an `i64`, so a
/// `number` with a fraction is an error, not truncated).
const VELT_GLOBAL_MODULES: &[&str] = &["std/text/string_builder"];

impl<'m> Ctx<'m> {
    /// Is the function `f` (fully qualified name, module) part of the JS API the standard
    /// library provides? Everything in a prelude module is, but for `VELT_PRELUDE`. In a module the prelude's globals
    /// re-export from (`velt/net/http` and its submodules such as `std/fetch/response`, `velt/net/url`,
    /// `velt/task`), the methods are (of the re-exported classes and the classes they use), and
    /// the top-level functions the globals re-export (`fetch`), but not its other functions:
    /// `velt/task`'s `workerCount()` is a Velt API whether or not the program names
    /// `AbortController`, which loads that module's global.
    pub(crate) fn is_js_api_fn(&self, module: usize, name: &str) -> bool {
        let Some(names) = self.js_api_modules().get(&module) else {
            return false;
        };
        let Some(names) = names else {
            return true;
        };
        let local = name.rsplit_once("::").map_or(name, |(_, n)| n);
        local.contains('.') || names.contains(local)
    }

    fn js_api_modules(&self) -> &FxHashMap<usize, Option<FxHashSet<String>>> {
        let modules = self.modules;
        self.js_api_modules.get_or_init(|| {
            // For each module path a global re-exports from: the names it re-exports.
            let mut reexported: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
            for g in modules
                .iter()
                .filter(|g| g.is_std && g.path.starts_with("std/prelude/global/"))
            {
                for item in &g.ast.items {
                    let velt_syntax::ast::ItemKind::Import(imp) = &item.kind else {
                        continue;
                    };
                    let Some((_, path)) = g.imports.iter().find(|(spec, _)| *spec == imp.from)
                    else {
                        continue;
                    };
                    let names = reexported.entry(path.as_str()).or_default();
                    names.extend(imp.names.iter().map(|n| n.name.name.clone()));
                }
            }
            let mut out = FxHashMap::default();
            for (i, m) in modules.iter().enumerate() {
                if !m.is_std || VELT_GLOBAL_MODULES.contains(&m.path.trim_end_matches(".vlt")) {
                    continue;
                }
                if m.path.starts_with("std/prelude/") {
                    if !VELT_PRELUDE.contains(&m.path.trim_end_matches(".vlt")) {
                        out.insert(i, None);
                    }
                    continue;
                }
                let names: FxHashSet<String> = reexported
                    .iter()
                    .filter(|(g, _)| m.path == **g || m.path.starts_with(&format!("{g}/")))
                    .flat_map(|(_, n)| n.iter().cloned())
                    .collect();
                if reexported
                    .keys()
                    .any(|g| m.path == *g || m.path.starts_with(&format!("{g}/")))
                {
                    out.insert(i, Some(names));
                }
            }
            out
        })
    }
}
