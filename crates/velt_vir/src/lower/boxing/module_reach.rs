//! The types whose values module state can reach, and the parameter attributes that cannot
//! hold for them (param_attrs.rs "Module state"): a function the call reaches may write such a
//! value through the module variable that holds it, so a param of its type is neither
//! `noalias` nor `frozen`.
//!
//! As in transfers.rs, what a function value, an interface value or a class with subclasses
//! holds is not in its static type, so the walk goes on through what this pass actually built
//! (`Facts`): the capture types of every closure environment, the concrete types put into
//! interface and dynamic values, and the concrete classes of each hierarchy. That is why it
//! runs once the pass has built every function.

use rustc_hash::FxHashSet;

use velt_sema::hir::{DefId, TyId, TyKind};

use crate::lower::Cx;
use crate::vir::FuncId;

impl Cx<'_> {
    /// Record that param `index` of the function being declared got `noalias` or `frozen` at
    /// type `ty` (`unfreeze_module_reach` checks it once the pass is complete).
    pub(in crate::lower) fn note_unique_param(&mut self, index: usize, ty: TyId) {
        self.declared_unique.push((index as u32, ty));
    }

    /// Associate the params `note_unique_param` recorded for the function just built with it.
    pub(in crate::lower) fn bind_unique_params(&mut self, f: FuncId) {
        let declared = std::mem::take(&mut self.declared_unique);
        (self.unique_params).extend(declared.into_iter().map(|(i, t)| (f, i, t)));
    }

    /// Drop `noalias` and `frozen` from every param whose type a value in module state can be
    /// or contain. Called once this pass has built every function, so the facts are complete.
    pub(in crate::lower) fn unfreeze_module_reach(&mut self) {
        let params = std::mem::take(&mut self.unique_params);
        let Some(state) = self.hir.module_state.as_ref().map(|m| m.ty) else {
            return;
        };
        if params.is_empty() {
            return;
        }
        let (types, roots) = self.module_reach(state);
        for (f, i, t) in params {
            let reached = types.contains(&t)
                || matches!(self.kind(t), TyKind::Adt(d, _)
                    if self.is_class(t) && roots.contains(&self.class_root(d)));
            if !reached {
                continue;
            }
            if let Some(attrs) = (self.funcs[f.0 as usize].as_mut())
                .and_then(|func| func.param_attrs.get_mut(i as usize))
            {
                attrs.noalias = false;
                attrs.frozen = false;
            }
        }
    }

    /// The types of the values reachable from a value of `state` (the module state tuple),
    /// and the class hierarchies (by root) whose objects are among them.
    fn module_reach(&mut self, state: TyId) -> (FxHashSet<TyId>, FxHashSet<DefId>) {
        let mut seen = FxHashSet::default();
        let mut roots = FxHashSet::default();
        let mut work = vec![state];
        let (mut captures_done, mut impls_done) = (false, false);
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            match self.kind(t) {
                // A function value may be any closure of the program.
                TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                    if !std::mem::replace(&mut captures_done, true) {
                        work.extend(self.facts.captures.iter().copied());
                    }
                }
                // An interface or dynamic value may hold any type a table was built for.
                TyKind::Dyn(..) => {
                    if !std::mem::replace(&mut impls_done, true) {
                        work.extend(self.facts.vtable_types.iter().copied());
                        work.extend(self.facts.dyn_types.iter().copied());
                    }
                }
                TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => work.push(e),
                TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
                    work.extend([a, b]);
                }
                TyKind::Adt(d, args) => {
                    work.extend(self.part_types(t));
                    // What a standard-library container keeps behind its own pointers.
                    work.extend(args);
                    if self.is_class(t) && roots.insert(self.class_root(d)) {
                        // It may hold any class of its hierarchy that was built.
                        let root = self.class_root(d);
                        let classes: Vec<TyId> = self.facts.vtable_types.iter().copied().collect();
                        work.extend(classes.into_iter().filter(|&c| {
                            matches!(self.kind(c), TyKind::Adt(cd, _)
                                if self.is_class(c) && self.class_root(cd) == root)
                        }));
                    }
                }
                _ => work.extend(self.part_types(t)),
            }
        }
        (seen, roots)
    }
}
