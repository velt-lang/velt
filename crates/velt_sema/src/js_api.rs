//! The JS API the standard library provides (`body::expr::numbers::is_js_api`): user code sees
//! its integer results as `number`, like TypeScript.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::SourceModule;

/// The prelude modules that are Velt's own API, JS having no counterpart: the ordering protocol
/// (`compareTo`, classes.md) and the integer bit counts. Their integer results stay integers in
/// user code (`x.trailingZeros()` indexes an array as it is). A Velt-only prelude method goes in
/// one of them, or in a new module listed here.
const VELT_PRELUDE: &[&str] = &[
    velt_common::std_module!(prelude_compare),
    velt_common::std_module!(prelude_bits),
];

/// The modules of Velt-only globals (no JavaScript counterpart): like `VELT_PRELUDE`, their
/// integer parameters and results stay integers (`StringBuilder.appendInt` takes an `i64`, so a
/// `number` with a fraction is an error, not truncated).
const VELT_GLOBAL_MODULES: &[&str] = &[velt_common::std_module!(text_string_builder)];

impl<'m> Ctx<'m> {
    /// Is the function `f` (fully qualified name, module) part of the JS API the standard
    /// library provides? Everything in a prelude module or a compiler hook's (std/lang/hooks) is,
    /// but for `VELT_PRELUDE`. In a module the prelude's globals
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
        let modules = self.modules.clone();
        self.js_api_modules.get_or_init(|| {
            // For each module path a global re-exports from: the names it re-exports.
            let mut reexported: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
            for g in modules
                .iter()
                .filter(|g| g.is_std && g.path.starts_with(velt_common::std_dir!(prelude_global)))
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
                    // And the module that defines each name, through std's own re-exports
                    // (`fetch` → velt/net/http/fetch/fetch → `Response` from fetch/response).
                    for n in &imp.names {
                        if let Some(def) = defining_module(&modules, path, &n.name.name) {
                            reexported
                                .entry(def)
                                .or_default()
                                .insert(n.name.name.clone());
                        }
                    }
                }
            }
            let mut out = FxHashMap::default();
            for (i, m) in modules.iter().enumerate() {
                if !m.is_std || VELT_GLOBAL_MODULES.contains(&m.path.trim_end_matches(".vlt")) {
                    continue;
                }
                // The compiler's hooks were prelude functions, and calls of them in a program's
                // code (the code sema writes) keep the prelude's conversions.
                if m.path.starts_with(velt_common::std_dir!(prelude))
                    || m.path.starts_with(velt_common::std_dir!(lang_hooks))
                {
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

/// The std module that defines `name`, starting at the module `path` and following its
/// re-exports (`export { name } from "…"`); `None` when that module is not loaded.
fn defining_module<'m>(modules: &[&'m SourceModule], path: &'m str, name: &str) -> Option<&'m str> {
    let mut path = path;
    let mut name = name.to_string();
    // A re-export chain is short; the bound only stops a cycle.
    for _ in 0..16 {
        let m = modules.iter().find(|m| m.path == path && m.is_std)?;
        let next = m.ast.items.iter().find_map(|item| match &item.kind {
            velt_syntax::ast::ItemKind::Import(imp) if item.exported && !imp.from.is_empty() => {
                let n = imp
                    .names
                    .iter()
                    .find(|n| n.alias.as_ref().unwrap_or(&n.name).name == name)?;
                let (_, target) = m.imports.iter().find(|(spec, _)| *spec == imp.from)?;
                Some((target.as_str(), n.name.name.clone()))
            }
            _ => None,
        });
        match next {
            Some((target, original)) => {
                path = target;
                name = original;
            }
            None => return Some(path),
        }
    }
    None
}
