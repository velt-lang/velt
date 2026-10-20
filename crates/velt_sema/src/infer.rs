//! Type-argument inference: match a generic signature type (whose `Param(i)` are the callee's
//! type parameters, "slots") against the type of an argument / the expected type.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{DefId, ImplDef, TyId, TyKind, TyOp};

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
            (TyKind::Adt(d, ps), TyKind::Adt(d2, as_)) if self.same_anon_shape(d, d2) => {
                // Two anonymous defs of one shape (`{ v: T0; n: i64 }` and `{ v: string; n: i64 }`,
                // crate::anon): match field by field.
                let pf: Vec<TyId> = self.anon_field_tys(d);
                let af: Vec<TyId> = self.anon_field_tys(d2);
                let pf: Vec<TyId> = pf.iter().map(|t| self.subst(*t, &ps)).collect();
                let af: Vec<TyId> = af.iter().map(|t| self.subst(*t, &as_)).collect();
                self.match_all(&pf, &af, slots)
            }
            (TyKind::Adt(..), TyKind::Adt(..)) => match self.base_of(actual) {
                Some(b) => self.match_ty(pat, b, slots),
                None => false,
            },
            (TyKind::Dyn(d, ps), TyKind::Dyn(d2, as_)) if d == d2 => {
                self.match_iface_args(&ps, &as_, slots)
            }
            (TyKind::Dyn(d, ps), _) => match self.impl_args(actual, d) {
                Some(as_) => self.match_iface_args(&ps, &as_, slots),
                None => false,
            },
            (TyKind::Array(p), TyKind::Array(a))
            | (TyKind::Option(p, _), TyKind::Option(a, _))
            | (TyKind::Shared(p), TyKind::Shared(a)) => self.match_ty(p, a, slots),
            (TyKind::Promise(p, pe), TyKind::Promise(a, ae)) => {
                let x = self.match_ty(p, a, slots);
                self.match_error(pe, ae, slots) && x
            }
            (TyKind::Option(p, _), _) => self.match_ty(p, actual, slots),
            (TyKind::Result(p1, p2), TyKind::Result(a1, a2))
            | (TyKind::Map(p1, p2), TyKind::Map(a1, a2)) => {
                let x = self.match_ty(p1, a1, slots);
                self.match_ty(p2, a2, slots) && x
            }
            (TyKind::Tuple(ps), TyKind::Tuple(as_)) => self.match_all(&ps, &as_, slots),
            (TyKind::Op(TyOp::Intersect, ps), TyKind::Op(TyOp::Intersect, as_))
                if ps.len() == as_.len() =>
            {
                self.match_all(&ps, &as_, slots)
            }
            (TyKind::Op(TyOp::Intersect, parts), _) => {
                self.match_intersection(&parts, actual, slots)
            }
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

    /// A deferred intersection `T & { … }` against an object type (#350). The object parts' fields
    /// match the argument's fields of the same names first (`$set: (v: T) => void` of
    /// `Signal<T>` against a `Signal<{ n: number }>` binds `T = { n: number }`); a type parameter
    /// part still unbound then takes the whole argument, as TypeScript infers it from an object
    /// literal (`T = { a: number; id: string }` for `{ a: 1, id: "x" }` against
    /// `T & { id: string }`), or, for an argument whose type an intersection made, the
    /// intersection of its parts that are not parts of the pattern (`T = A` for an
    /// `A & { id: string }`, as TypeScript infers it).
    fn match_intersection(
        &mut self,
        parts: &[TyId],
        actual: TyId,
        slots: &mut [Option<TyId>],
    ) -> bool {
        // `Signal<T & object>`: the nested intersection's parts are parts of this one, and an
        // `object` part binds nothing.
        let mut flat = vec![];
        for &p in parts {
            match self.ty.kind(p) {
                TyKind::Op(TyOp::Intersect, ps) => flat.extend(ps.iter().copied()),
                _ => flat.push(p),
            }
        }
        let all = flat.len();
        flat.retain(|&p| self.dynamic(p) != Some(crate::any::Dynamic::Object));
        // `T & object` against a class instance, an array or a `Map`: `T` is the argument, as
        // TypeScript infers it.
        if let [p] = flat[..] {
            if let TyKind::Param(i) = *self.ty.kind(p) {
                let i = i as usize;
                if flat.len() < all
                    && !self.is_object_type(actual)
                    && self.is_object_ty(actual)
                    && !self.ty.has_error(actual)
                {
                    if i < slots.len() && slots[i].is_none() {
                        slots[i] = Some(actual);
                    }
                    return i >= slots.len() || slots[i] == Some(actual);
                }
            }
        }
        if let Some((ifaces, object)) = self.meet_parts(actual) {
            return self.match_iface_intersection(&flat, ifaces, object, slots);
        }
        if !self.is_object_type(actual) {
            return false;
        }
        let parts = &flat[..];
        let fields = self.public_fields(actual);
        let mut ok = true;
        for &p in parts {
            if !self.is_object_type(p) {
                continue;
            }
            for f in self.public_fields(p) {
                if let Some(a) = fields.iter().find(|a| a.name == f.name) {
                    ok &= self.match_ty(f.ty, a.ty, slots);
                }
            }
        }
        for &p in parts {
            if let TyKind::Param(i) = self.ty.kind(p) {
                let i = *i as usize;
                if i < slots.len() && slots[i].is_none() && !self.ty.has_error(actual) {
                    slots[i] = Some(self.unmatched_parts(parts, actual));
                }
            }
        }
        ok
    }

    /// A deferred intersection's `parts` against an interface made of an intersection
    /// (`crate::synth_ifaces`) of the interface values `ifaces` and the object type `object`:
    /// the object parts' fields match its object part's, and a type parameter part takes
    /// the rest (`T = Named` for a `Named & { $set }` against `T & { $set }`).
    fn match_iface_intersection(
        &mut self,
        parts: &[TyId],
        ifaces: Vec<TyId>,
        object: Option<TyId>,
        slots: &mut [Option<TyId>],
    ) -> bool {
        let fields = object.map_or(vec![], |o| self.public_fields(o));
        let mut ok = true;
        let mut covered: Vec<String> = vec![];
        for &p in parts {
            if !self.is_object_type(p) {
                continue;
            }
            for f in self.public_fields(p) {
                if let Some(a) = fields.iter().find(|a| a.name == f.name) {
                    ok &= self.match_ty(f.ty, a.ty, slots);
                    covered.push(f.name);
                }
            }
        }
        let mut rest: Vec<TyId> = ifaces.into_iter().filter(|i| !parts.contains(i)).collect();
        let left: Vec<_> = fields
            .into_iter()
            .filter(|f| !covered.contains(&f.name))
            .collect();
        if !left.is_empty() {
            rest.push(self.anon_type_with(&left, 0));
        }
        let Some((&first, others)) = rest.split_first() else {
            return ok;
        };
        let mut acc = first;
        for &o in others {
            match self.meet(acc, o, 0, 0) {
                Ok(m) => acc = m,
                Err(_) => return false,
            }
        }
        for &p in parts {
            if let TyKind::Param(i) = self.ty.kind(p) {
                let i = *i as usize;
                if i < slots.len() && slots[i].is_none() {
                    slots[i] = Some(acc);
                }
            }
        }
        ok
    }

    /// `actual` without the parts of the intersection that made it which are parts of the
    /// pattern `parts` too; `actual` when no intersection made it, or nothing is left.
    fn unmatched_parts(&mut self, parts: &[TyId], actual: TyId) -> TyId {
        let Some(declared) = self.type_level.intersection_parts.get(&actual).cloned() else {
            return actual;
        };
        let all = declared.len();
        let rest: Vec<TyId> = declared
            .into_iter()
            .filter(|&q| !parts.iter().any(|&p| self.same_object(p, q)))
            .collect();
        // No part of the pattern is a part of the argument (or every one is): the whole argument.
        let Some((&first, others)) = rest.split_first().filter(|_| rest.len() < all) else {
            return actual;
        };
        let mut acc = first;
        for &o in others {
            match self.meet(acc, o, 0, 0) {
                Ok(m) => acc = m,
                Err(_) => return actual,
            }
        }
        acc
    }

    /// Are `a` and `b` one object type by structure: the same type, or object types (an
    /// interface's or written out) with the same fields, each of the same type and optionality?
    fn same_object(&mut self, a: TyId, b: TyId) -> bool {
        if a == b {
            return true;
        }
        if !self.is_object_type(a) || !self.is_object_type(b) {
            return false;
        }
        let (fa, fb) = (self.public_fields(a), self.public_fields(b));
        fa.len() == fb.len()
            && fa.iter().all(|f| {
                fb.iter()
                    .any(|g| g.name == f.name && g.ty == f.ty && g.optional == f.optional)
            })
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
        // A union against a union (`{ ok: true; v: T } | { ok: false; e: string }` against the
        // same union at `T = i64`): every member of `actual` matches some member of `pat`.
        if let Some(actual_members) = self.union_members(actual) {
            let mut trial = slots.to_vec();
            let all = actual_members.iter().all(|a| {
                members.iter().any(|m| {
                    let mut t = trial.clone();
                    let ok = self.match_ty(*m, *a, &mut t);
                    if ok {
                        trial = t;
                    }
                    ok
                })
            });
            if all {
                slots.copy_from_slice(&trial);
                return true;
            }
        }
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

    /// Interface arguments: an argument may be an error type (`E` of `Iterable<T, E>`), so
    /// `never` binds a parameter there, as in [`Self::match_error`].
    fn match_iface_args(&mut self, ps: &[TyId], as_: &[TyId], slots: &mut [Option<TyId>]) -> bool {
        if ps.len() != as_.len() {
            return false;
        }
        let mut ok = true;
        for (p, a) in ps.iter().zip(as_) {
            ok &= self.match_error(*p, *a, slots);
        }
        ok
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
        match self.dynamic_takes(iface, ty) {
            Some(true) => return self.any_impl().map(|i| (i, vec![], ty)),
            Some(false) => return None,
            None => {}
        }
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
        if self.field_only.contains_key(&iface) {
            return self.field_only_impl(ty, iface);
        }
        self.synth_impl(ty, iface)
    }

    /// `(impl index, interface args, t)` if impl `i` is for type `t`.
    fn try_impl(&mut self, i: u32, t: TyId) -> Option<(u32, Vec<TyId>, TyId)> {
        let imp = &self.impls[i as usize];
        let (pat, n, iargs) = (imp.ty, imp.generics as usize, imp.iface_args.clone());
        let mut slots = vec![None; n];
        // `impl C<T, E>` binds its params to the type's own args, `never` included (`match_ty`
        // lets `never` match anything without binding: `Generator<i64, never>`).
        if let (TyKind::Adt(d, ps), TyKind::Adt(d2, as_)) =
            (self.ty.kind(pat).clone(), self.ty.kind(t).clone())
        {
            if d == d2 && ps.len() == as_.len() {
                for (p, a) in ps.iter().zip(&as_) {
                    if let TyKind::Param(i) = self.ty.kind(*p) {
                        if let Some(s @ None) = slots.get_mut(*i as usize) {
                            *s = Some(*a);
                        }
                    }
                }
            }
        }
        if !(self.match_ty(pat, t, &mut slots) && self.subst_known(pat, &slots) == t) {
            return None;
        }
        let args = slots
            .iter()
            .map(|s| s.unwrap_or(self.ty.error))
            .collect::<Vec<_>>();
        let iargs = iargs.iter().map(|a| self.subst(*a, &args)).collect();
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
                args: p.args.iter().map(|t| self.subst(*t, &b.args)).collect(),
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
    by_iface: FxHashMap<DefId, IfaceImpls>,
}

#[derive(Default)]
struct IfaceImpls {
    /// Non-generic impls: the type → its first impl (the only type such an impl fits).
    exact: FxHashMap<TyId, u32>,
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
