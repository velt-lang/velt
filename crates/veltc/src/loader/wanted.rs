//! Which re-exports of a std module the program needs (docs/internals/design/std-layout.md §2).
//!
//! A module's `index.vlt` is its public API: named re-exports of the files that implement it
//! (`export { Deque } from "velt/collections/deque";`). A re-export binds nothing in the module
//! itself, so it only has to be loaded when some module imports one of its names: a program
//! that imports `Deque` from `velt/collections` loads `collections/deque.vlt`, not the B+ tree
//! and the concurrent map next to it. The loader records what each importer asks of a std
//! module ([`Wanted`]) and follows a std module's re-exports only for those names; the module's
//! own imports, and `export * from`, are always followed. A re-export that is followed asks its
//! target for every name it lists (sema checks them all). A target with effects beyond its
//! exports is always loaded ([`has_effects`]): `extend` blocks apply wherever their module is
//! loaded (velt/simd's methods on the built-in vector types), as do augmentations and
//! top-level statements. A module nobody asked anything of (the
//! root, the prelude, a global, an extra root) wants everything.

use std::collections::{HashMap, HashSet};

use velt_common::Span;
use velt_syntax::ast;

use super::{file_key, resolve};

/// What importers ask of one module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Wanted {
    /// Everything: `import * as ns`, `import "x"`, `export * from`, or a module no one imported.
    All,
    /// These exported names (as the module exports them, before any `as`).
    Names(HashSet<String>),
}

/// The names `imp` takes from its module, or `None` for all of them.
pub(super) fn names_taken(imp: &ast::Import) -> Option<Vec<String>> {
    if imp.all || imp.namespace.is_some() || imp.names.is_empty() {
        return None;
    }
    Some(imp.names.iter().map(|n| n.name.name.clone()).collect())
}

/// Whether loading `module` changes more than what it exports: `extend` blocks, `declare
/// global` / `declare module` augmentations, top-level statements, or a side-effect import.
pub(super) fn has_effects(module: &ast::Module) -> bool {
    module.items.iter().any(|item| match &item.kind {
        ast::ItemKind::Extend(_) | ast::ItemKind::Augment(_) => true,
        // Top-level statements are parsed into a synthesized function (`velt_sema`'s
        // `module_scripts`): the script's main, or a chunk with an empty name span.
        ast::ItemKind::Function(f) => {
            velt_syntax::is_script_main(&f.sig.name) || f.sig.name.span.lo == f.sig.name.span.hi
        }
        ast::ItemKind::Import(imp) => {
            !item.exported
                && imp.names.is_empty()
                && imp.namespace.is_none()
                && !imp.from.is_empty()
        }
        _ => false,
    })
}

/// [`Wanted`] per module index.
#[derive(Default)]
pub(super) struct WantedByModule(HashMap<usize, Wanted>);

impl WantedByModule {
    /// Record that an importer takes `names` (`None`: all) from module `target`; whether that
    /// asks for something the module was not asked for before.
    pub(super) fn request(&mut self, target: usize, names: Option<Vec<String>>) -> bool {
        match (self.0.get_mut(&target), names) {
            (Some(Wanted::All), _) => false,
            (Some(w), None) => {
                *w = Wanted::All;
                true
            }
            (Some(Wanted::Names(have)), Some(names)) => {
                let before = have.len();
                have.extend(names);
                have.len() > before
            }
            (None, names) => {
                let wanted = match names {
                    None => Wanted::All,
                    Some(names) => Wanted::Names(names.into_iter().collect()),
                };
                self.0.insert(target, wanted);
                true
            }
        }
    }

    /// Whether module `module`'s re-export `imp` is needed: it names something asked for.
    pub(super) fn needs(&self, module: usize, imp: &ast::Import) -> bool {
        match (self.0.get(&module), names_taken(imp)) {
            (None | Some(Wanted::All), _) | (_, None) => true,
            (Some(Wanted::Names(have)), Some(listed)) => listed.iter().any(|n| {
                // `export { a as b }` exports `b`.
                let exported = imp
                    .names
                    .iter()
                    .find(|x| x.name.name == *n)
                    .and_then(|x| x.alias.as_ref())
                    .map_or(n.as_str(), |a| a.name.as_str());
                have.contains(exported)
            }),
        }
    }
}

impl super::Loader<'_, '_> {
    /// Whether the std module `spec` names from module `index` may be left unloaded: it is not
    /// loaded yet and has no effects beyond its exports ([`has_effects`]).
    pub(super) fn pure(&mut self, index: usize, spec: &str) -> bool {
        let memo = (index, spec.to_string());
        let key = match self.skipped.get(&memo) {
            Some(key) => key.clone(),
            None => {
                let importer = &self.origins[index].0;
                let std = self.opts.std_root.as_deref();
                let found = resolve::resolve_module(spec, importer, std, None);
                let found = found.map(|f| (file_key(&f), f));
                self.skipped.insert(memo, found.clone());
                found
            }
        };
        let Some((key, file)) = key else {
            return false;
        };
        if self.by_file.contains_key(&key) {
            return false;
        }
        if let Some(&effects) = self.effects.get(&key) {
            return !effects;
        }
        let effects = match self.read(&file) {
            Ok(src) => {
                let (module, _) = velt_syntax::parse_file(velt_common::FileId(u32::MAX), &src);
                has_effects(&module)
            }
            Err(_) => true,
        };
        self.effects.insert(key, effects);
        !effects
    }
}

/// `declare module "spec" { … }` as the import that loads the augmented module (everything).
pub(super) fn augmented(spec: &str, span: Span) -> ast::Import {
    ast::Import {
        names: vec![],
        from: spec.to_string(),
        from_span: span,
        namespace: None,
        all: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    fn import(src: &str) -> ast::Import {
        let (module, diags) = velt_syntax::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        match &module.items[0].kind {
            ast::ItemKind::Import(imp) => imp.clone(),
            other => panic!("not an import: {other:?}"),
        }
    }

    #[test]
    fn a_re_export_is_needed_only_for_names_asked_for() {
        let deque = import("export { Deque } from \"velt/collections/deque\";");
        let btree = import("export { BTreeMap, InlineKey } from \"velt/collections/btree_map\";");
        let mut w = WantedByModule::default();
        assert!(
            w.needs(1, &deque),
            "a module no one imported wants everything"
        );
        assert!(w.request(1, Some(vec!["Deque".into()])));
        assert!(w.needs(1, &deque));
        assert!(!w.needs(1, &btree));
        assert!(!w.request(1, Some(vec!["Deque".into()])), "nothing new");
        assert!(w.request(1, Some(vec!["InlineKey".into()])));
        assert!(w.needs(1, &btree));
        assert!(w.request(1, None));
        assert_eq!(w.0[&1], Wanted::All);
        assert!(!w.request(1, Some(vec!["X".into()])));
    }

    #[test]
    fn renamed_re_exports_match_the_exported_name() {
        let renamed = import("export { inner as outer } from \"velt/x/inner\";");
        let mut w = WantedByModule::default();
        w.request(2, Some(vec!["outer".into()]));
        assert!(w.needs(2, &renamed));
        let mut w = WantedByModule::default();
        w.request(2, Some(vec!["inner".into()]));
        assert!(!w.needs(2, &renamed));
    }

    #[test]
    fn extend_blocks_augmentations_and_side_effect_imports_are_effects() {
        let effects = |src: &str| {
            let (module, diags) = velt_syntax::parse_file(FileId(0), src);
            assert!(diags.is_empty(), "{diags:?}");
            has_effects(&module)
        };
        assert!(effects(
            "extend u64x2 { lane(i: i64): u64 { return 0; } }\n"
        ));
        assert!(effects("import \"velt/simd/sha\";\n"));
        assert!(effects("declare global { interface Window { x: i64; } }\n"));
        assert!(!effects(
            "import { a } from \"./a\";\nexport function f(): i64 { return 1; }\n"
        ));
        assert!(!effects(
            "export { Deque } from \"velt/collections/deque\";\n"
        ));
    }

    #[test]
    fn namespaces_side_effects_and_star_take_everything() {
        assert_eq!(
            names_taken(&import("import * as c from \"velt/collections\";")),
            None
        );
        assert_eq!(names_taken(&import("import \"velt/collections\";")), None);
        assert_eq!(
            names_taken(&import("export * from \"velt/collections\";")),
            None
        );
        assert_eq!(
            names_taken(&import(
                "import { Deque, type SortedMap } from \"velt/collections\";"
            )),
            Some(vec!["Deque".into(), "SortedMap".into()])
        );
    }
}
