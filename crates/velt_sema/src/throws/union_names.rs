//! Union names show a function type written without `throws` as written, its error type left
//! out (`crate::display`), with or without error-set variables (`throws::vars`): a check
//! without them shows the type it resolves to with its inferred set so (`Types::inferred_fns`),
//! and a check with them the solved form of the type with variables. A union def is named when
//! it is first interned, which may be before such a type is known to be one, so the union defs
//! whose members show one are named again once every body is checked
//! ([`Ctx::name_unions_as_written`]): their names (which lowering names layouts by) do not
//! depend on the order types were met in.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// A function type written without `throws` is shown as written, with or without
    /// variables: the type it resolves to with its inferred set (`Types::inferred_fns`), and
    /// the solved form of one with variables (`solved`, [`Ctx::solved_types`]). A union def is
    /// named when it is first interned, which may be before such a type is known to be one
    /// (the same type written with its `throws`, in a signature collected before the site): the
    /// union defs whose members hold one are named again once every body is checked, so their
    /// names do not depend on that order.
    pub(crate) fn name_unions_as_written(&mut self, solved: &[(TyId, TyId)]) {
        for &(t, s) in solved {
            if t != s && self.shown_as_written(t) {
                self.ty.inferred_fns.insert(s);
            }
        }
        // An anonymous object type's fields with variables are their solved types, as erasing
        // makes them (`crate::readonly::erase`): it is shown by them, as the unions holding it.
        for i in 0..self.info.len() {
            let d = crate::hir::DefId(i as u32);
            let Some(a) = self.adt(d).filter(|a| a.kind == crate::hir::AdtKind::Anon) else {
                continue;
            };
            if solved.is_empty() || !a.generics.names.is_empty() {
                continue;
            }
            let tys: Vec<(TyId, TyId)> = a.fields.iter().map(|f| (f.ty, f.declared)).collect();
            if !tys
                .iter()
                .any(|&(t, w)| self.mentions_err_var(t) || self.mentions_err_var(w))
            {
                continue;
            }
            let tys: Vec<(TyId, TyId)> = (tys.into_iter())
                .map(|(t, w)| (self.solved_type(t), self.solved_type(w)))
                .collect();
            if let DefInfo::Adt(a) = &mut self.info[i] {
                for (f, (t, w)) in a.fields.iter_mut().zip(tys) {
                    f.ty = t;
                    f.declared = w;
                }
            }
        }
        if self.ty.inferred_fns.is_empty() {
            return;
        }
        let marked = self.ty.inferred_fns.clone();
        for i in 0..self.info.len() {
            let d = crate::hir::DefId(i as u32);
            let Some(e) = self
                .enum_info(d)
                .filter(|e| e.is_union && e.generics.names.is_empty())
            else {
                continue;
            };
            let payloads: Vec<TyId> = (e.variants.iter())
                .filter_map(|v| v.payload.first().copied())
                .collect();
            if payloads.len() != e.variants.len()
                || !payloads.iter().any(|p| self.holds_any(*p, &marked))
            {
                continue;
            }
            // A union named by its alias (`type Tree = …`) keeps that name; its variants are
            // named again.
            let shown: Vec<&str> = e.variants.iter().map(|v| v.name.as_str()).collect();
            let named =
                (e.name != shown.join(" | ")).then(|| (e.name.clone(), e.qual_name.clone()));
            self.reorder_union_def(d, &payloads);
            let DefInfo::Enum(e) = &mut self.info[i] else {
                continue;
            };
            if let Some((name, qual)) = named {
                e.name = name;
                e.qual_name = qual;
            }
            let (name, names): (String, Vec<String>) = (
                e.qual_name.clone(),
                e.variants.iter().map(|v| v.name.clone()).collect(),
            );
            if let Some(Some(crate::hir::Def::Enum(h))) = self.defs.get_mut(i) {
                for (v, n) in h.variants.iter_mut().zip(names) {
                    v.name = n;
                }
                h.name = name;
            }
        }
    }

    /// Is function type `t` shown as written, without its error type (`crate::display`)?
    fn shown_as_written(&self, t: TyId) -> bool {
        let TyKind::FnPtr { ret, throws, .. } = self.ty.kind(t) else {
            return false;
        };
        self.ty.inferred_fns.contains(&t)
            || self.only_err_vars(*throws)
            || matches!(self.ty.kind(*ret), TyKind::Promise(_, e) if self.only_err_vars(*e))
    }

    /// Does `t` show one of `tys` (itself, in its component types, or in the fields of an
    /// anonymous object type, which is shown by them)?
    fn holds_any(&self, t: TyId, tys: &rustc_hash::FxHashSet<TyId>) -> bool {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![t];
        while let Some(t) = stack.pop() {
            if tys.contains(&t) {
                return true;
            }
            if !seen.insert(t) {
                continue;
            }
            let k = self.ty.kind(t);
            stack.extend(crate::types::children(k));
            if let TyKind::Adt(d, _) = k {
                if let Some(a) = self.adt(*d).filter(|a| a.kind == crate::hir::AdtKind::Anon) {
                    stack.extend(a.fields.iter().flat_map(|f| [f.ty, f.declared]));
                }
            }
        }
        false
    }
}
