//! Type-argument inference: match a generic signature type (whose `Param(i)` are the callee's
//! type parameters, "slots") against the type of an argument / the expected type.

use std::collections::HashMap;

use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{DefId, ImplDef, TyId, TyKind};

impl Ctx<'_> {
    /// [`match_ty`](Self::match_ty) against an expected type whose error types are unknown
    /// ([`without_error_types`](crate::types::Types::without_error_types)): a slot may then be a
    /// type that is unknown only in its error types (`T` = `(x: i32) => i32 throws ?` for
    /// `const f: (x: i32) => i32 = id((x) => x + 1)`), so a closure argument of type `T` is
    /// checked against it and infers its own error type, as in TS.
    pub fn match_context(&mut self, pat: TyId, actual: TyId, slots: &mut [Option<TyId>]) {
        self.matching_context = true;
        self.match_ty(pat, actual, slots);
        self.matching_context = false;
    }

    fn context_slot_ok(&self, actual: TyId) -> bool {
        self.matching_context && !self.ty.has_error_outside_error_types(actual)
    }

    /// Bind unknown slots in `pat` from `actual`. Returns false on a structural mismatch (the
    /// caller reports mismatches later, after coercions, so the result is advisory).
    pub fn match_ty(&mut self, pat: TyId, actual: TyId, slots: &mut [Option<TyId>]) -> bool {
        if self.ty.is_bottom(actual) {
            return true;
        }
        let pk = self.ty.kind(pat).clone();
        let ak = self.ty.kind(actual).clone();
        match (pk, ak) {
            (TyKind::Param(i), _) if (i as usize) < slots.len() => match slots[i as usize] {
                None => {
                    if !self.ty.has_error(actual) || self.context_slot_ok(actual) {
                        slots[i as usize] = Some(actual);
                    }
                    true
                }
                Some(t) => t == actual,
            },
            (TyKind::Adt(d, ps), TyKind::Adt(d2, as_)) if d == d2 => {
                self.match_all(&ps, &as_, slots)
            }
            (TyKind::Adt(..), _) if self.union_def(pat).is_some() => {
                self.match_union_member(pat, actual, slots)
            }
            (TyKind::Adt(..), TyKind::Adt(..)) => match self.base_of(actual) {
                Some(b) => self.match_ty(pat, b, slots),
                None => false,
            },
            (TyKind::Dyn(d, ps), TyKind::Dyn(d2, as_)) if d == d2 => {
                self.match_all(&ps, &as_, slots)
            }
            (TyKind::Dyn(d, ps), _) => match self.impl_args(actual, d) {
                Some(as_) => self.match_all(&ps, &as_, slots),
                None => false,
            },
            (TyKind::Array(p), TyKind::Array(a))
            | (TyKind::Option(p), TyKind::Option(a))
            | (TyKind::Shared(p), TyKind::Shared(a)) => self.match_ty(p, a, slots),
            (TyKind::Promise(p, pe), TyKind::Promise(a, ae)) => {
                let x = self.match_ty(p, a, slots);
                self.match_error(pe, ae, slots) && x
            }
            (TyKind::Option(p), _) => self.match_ty(p, actual, slots),
            (TyKind::Result(p1, p2), TyKind::Result(a1, a2))
            | (TyKind::Map(p1, p2), TyKind::Map(a1, a2)) => {
                let x = self.match_ty(p1, a1, slots);
                self.match_ty(p2, a2, slots) && x
            }
            (TyKind::Tuple(ps), TyKind::Tuple(as_)) => self.match_all(&ps, &as_, slots),
            (
                TyKind::FnPtr {
                    params: pp,
                    ret: pr,
                    throws: pt,
                },
                TyKind::FnPtr {
                    params: ap,
                    ret: ar,
                    throws: at,
                },
            ) => {
                let x = self.match_all(&pp, &ap, slots);
                let y = self.match_error(pt, at, slots);
                self.match_ty(pr, ar, slots) && x && y
            }
            _ => pat == actual,
        }
    }

    /// An error type (`throws` of a function type, rejection of a promise): unlike other
    /// positions, `never` binds a parameter (the function does not throw).
    fn match_error(&mut self, pat: TyId, actual: TyId, slots: &mut [Option<TyId>]) -> bool {
        match self.ty.kind(pat) {
            TyKind::Param(i) if actual == self.ty.never && (*i as usize) < slots.len() => {
                match slots[*i as usize] {
                    None => {
                        slots[*i as usize] = Some(actual);
                        true
                    }
                    Some(t) => t == actual,
                }
            }
            _ => self.match_ty(pat, actual, slots),
        }
    }

    /// A member value against a generic union pattern (`{ kind: "some"; value: 5 }` for
    /// `Opt<T>`): the first member (a bare `T` member last) that matches binds the slots.
    fn match_union_member(&mut self, pat: TyId, actual: TyId, slots: &mut [Option<TyId>]) -> bool {
        if let Some(done) = self.match_union_rest(pat, actual, slots) {
            return done;
        }
        let mut members = self.union_members(pat).unwrap_or_default();
        members.sort_by_key(|m| matches!(self.ty.kind(*m), TyKind::Param(_)));
        for m in members {
            let mut trial = slots.to_vec();
            if self.match_ty(m, actual, &mut trial) {
                slots.copy_from_slice(&trial);
                return true;
            }
        }
        false
    }

    /// `T | E` against a union `A | E`: the members of `actual` that `pat` names itself (`E`)
    /// are matched away and the one type parameter takes the rest (`T = A`), as in TS.
    fn match_union_rest(
        &mut self,
        pat: TyId,
        actual: TyId,
        slots: &mut [Option<TyId>],
    ) -> Option<bool> {
        let actual_members = self.union_members(actual)?;
        let members = self.union_members(pat)?;
        let (params, fixed): (Vec<TyId>, Vec<TyId>) = members
            .into_iter()
            .partition(|m| matches!(self.ty.kind(*m), TyKind::Param(_)));
        let [param] = params.as_slice() else {
            return None;
        };
        let rest: Vec<TyId> = actual_members
            .iter()
            .copied()
            .filter(|a| !fixed.contains(a))
            .collect();
        if rest.is_empty() || rest.len() == actual_members.len() {
            return None;
        }
        let t = match rest.as_slice() {
            [one] => *one,
            _ => self.union_of(&rest, false, velt_common::Span::DUMMY),
        };
        Some(self.match_ty(*param, t, slots))
    }

    fn match_all(&mut self, ps: &[TyId], as_: &[TyId], slots: &mut [Option<TyId>]) -> bool {
        if ps.len() != as_.len() {
            return false;
        }
        let mut ok = true;
        for (p, a) in ps.iter().zip(as_) {
            ok &= self.match_ty(*p, *a, slots);
        }
        ok
    }

    /// Interface args with which `ty` (or a base class of it) implements `iface`.
    pub fn impl_args(&mut self, ty: TyId, iface: crate::hir::DefId) -> Option<Vec<TyId>> {
        self.find_impl(ty, iface).map(|(_, args, _)| args)
    }

    /// `(impl index, interface args, implementing type)` for `ty` or its nearest base class.
    pub fn find_impl(
        &mut self,
        ty: TyId,
        iface: crate::hir::DefId,
    ) -> Option<(u32, Vec<TyId>, TyId)> {
        self.impl_index.extend(&self.impls);
        let mut cur = Some(ty);
        let mut guard = 0;
        while let Some(t) = cur {
            // The first impl (in `impls` order) that fits wins: a non-generic impl fits exactly
            // its own type, so only generic impls before it need matching.
            let (exact, generic) = self.impl_index.candidates(iface, t);
            for i in generic
                .into_iter()
                .take_while(|&i| exact.is_none_or(|e| i < e))
            {
                if let Some(found) = self.try_impl(i, t) {
                    return Some(found);
                }
            }
            if let Some(found) = exact.and_then(|e| self.try_impl(e, t)) {
                return Some(found);
            }
            guard += 1;
            cur = if guard > 64 { None } else { self.base_of(t) };
        }
        None
    }

    /// `(impl index, interface args, t)` if impl `i` is for type `t`.
    fn try_impl(&mut self, i: u32, t: TyId) -> Option<(u32, Vec<TyId>, TyId)> {
        let imp = &self.impls[i as usize];
        let (pat, n, iargs) = (imp.ty, imp.generics as usize, imp.iface_args.clone());
        let mut slots = vec![None; n];
        if !(self.match_ty(pat, t, &mut slots) && self.ty.subst_known(pat, &slots) == t) {
            return None;
        }
        let args = slots
            .iter()
            .map(|s| s.unwrap_or(self.ty.error))
            .collect::<Vec<_>>();
        let iargs = iargs.iter().map(|a| self.ty.subst(*a, &args)).collect();
        Some((i, iargs, t))
    }

    /// Every interface `b` extends (transitively), with `b`'s args substituted.
    pub fn iface_ancestors(&mut self, b: &Bound) -> Vec<Bound> {
        let parents = self
            .iface(b.iface)
            .map(|i| i.parents.clone())
            .unwrap_or_default();
        parents
            .into_iter()
            .map(|p| Bound {
                iface: p.iface,
                args: p.args.iter().map(|t| self.ty.subst(*t, &b.args)).collect(),
            })
            .collect()
    }

    /// Does type `ty` satisfy bound `b`? `bounds` are the bounds of the generic params in scope
    /// (a `Param` satisfies its own declared bounds).
    pub fn satisfies(&mut self, ty: TyId, b: &Bound, bounds: &[Vec<Bound>]) -> bool {
        if self.ty.is_bottom(ty) {
            return true;
        }
        if let TyKind::Param(n) = self.ty.kind(ty) {
            let declared = bounds.get(*n as usize).cloned().unwrap_or_default();
            return declared
                .iter()
                .any(|x| x == b || self.iface_ancestors(x).contains(b));
        }
        self.impl_args(ty, b.iface)
            .is_some_and(|args| args == b.args)
    }
}

/// `Ctx::impls` by interface, so an impl lookup doesn't scan every impl of the program.
/// Impls are only ever appended; [`ImplIndex::extend`] catches up with new ones.
#[derive(Default)]
pub(crate) struct ImplIndex {
    indexed: usize,
    by_iface: HashMap<DefId, IfaceImpls>,
}

#[derive(Default)]
struct IfaceImpls {
    /// Non-generic impls: the type → its first impl (the only type such an impl fits).
    exact: HashMap<TyId, u32>,
    /// Generic impls, in order.
    generic: Vec<u32>,
}

impl ImplIndex {
    fn extend(&mut self, impls: &[ImplDef]) {
        for (i, imp) in impls.iter().enumerate().skip(self.indexed) {
            let e = self.by_iface.entry(imp.iface).or_default();
            if imp.generics == 0 {
                e.exact.entry(imp.ty).or_insert(i as u32);
            } else {
                e.generic.push(i as u32);
            }
        }
        self.indexed = impls.len();
    }

    /// The non-generic impl of `iface` for `t`, and every generic impl of `iface`.
    fn candidates(&self, iface: DefId, t: TyId) -> (Option<u32>, Vec<u32>) {
        match self.by_iface.get(&iface) {
            Some(e) => (e.exact.get(&t).copied(), e.generic.clone()),
            None => (None, vec![]),
        }
    }
}
