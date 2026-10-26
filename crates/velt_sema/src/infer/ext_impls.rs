//! The impls of `extend … implements I` blocks (collect/ext_impls.rs) in impl lookup.
//!
//! A type's own impls, and those of its base classes, come first (`Ctx::find_impl`); only a type
//! without one gets an `extend` block's: the most specific block whose target and bounds fit it,
//! looked for on the type and then on each base class. Collect refuses two blocks that can both
//! fit one type where neither is more specific ([`Ctx::ext_overlap`]), so that block is unique.
//!
//! A block's bounds hold when the type meets them at all (infer/ext_exists.rs), whichever block
//! or impl gives it the interface: that answer does not depend on which lookups are running,
//! so the block chosen for a type is the same in every context, and lookups are remembered
//! (`Ctx::ext_memo`). Lowering decides the same way.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{DefId, TyId, TyKind};

impl Ctx<'_> {
    /// The most specific `extend … implements iface` impl that fits `ty`, or else its nearest
    /// base class (module docs).
    pub(super) fn find_ext_impl(
        &mut self,
        ty: TyId,
        iface: DefId,
    ) -> Option<(u32, Vec<TyId>, TyId)> {
        let cands = self.ext_by_iface.get(&iface).cloned()?;
        // A finished lookup of a type without parameters (whose answer doesn't depend on the
        // bounds in scope) is remembered while no impl is added.
        if self.ext_memo_impls != self.impls.len() {
            self.ext_memo.clear();
            self.ext_exists_memo.clear();
            self.ext_memo_impls = self.impls.len();
        }
        let closed = !self.mentions_params(ty);
        if let Some(found) = closed.then(|| self.ext_memo.get(&(ty, iface))).flatten() {
            return found.clone();
        }
        let mut cur = Some(ty);
        let mut best = None;
        for _ in 0..64 {
            let Some(t) = cur else { break };
            best = self.most_specific_ext(&cands, t);
            if best.is_some() {
                break;
            }
            cur = self.base_of(t);
        }
        if closed && self.ext_memo_impls == self.impls.len() {
            self.ext_memo.insert((ty, iface), best.clone());
        }
        best
    }

    /// Of the impls `cands` (`extend` blocks' impls of one interface, in order), the most
    /// specific that fits `t`.
    fn most_specific_ext(&mut self, cands: &[u32], t: TyId) -> Option<(u32, Vec<TyId>, TyId)> {
        let mut best: Option<(u32, Vec<TyId>, TyId)> = None;
        for &i in cands {
            // Two blocks that both fit `t` are ordered (collect/ext_impls.rs), so one that is
            // not more specific than the best so far is not the most specific.
            if best
                .as_ref()
                .is_some_and(|b| !self.ext_more_specific(i, b.0))
            {
                continue;
            }
            if let Some(f) = self.fitting_impl(i, t) {
                best = Some(f);
            }
        }
        best
    }

    /// `try_impl`, and for an `extend … implements` block's impl, its bounds hold too.
    pub(crate) fn fitting_impl(&mut self, i: u32, t: TyId) -> Option<(u32, Vec<TyId>, TyId)> {
        let found = self.try_impl(i, t)?;
        let Some(&e) = self.ext_impls.get(&i) else {
            return Some(found);
        };
        self.ext_bounds_hold(i, e, t).then_some(found)
    }

    /// Do the bounds of extension `e` hold for the type args with which its impl `i` fits `t`?
    fn ext_bounds_hold(&mut self, i: u32, e: usize, t: TyId) -> bool {
        let Some(obligations) = self.ext_obligations(i, e, t) else {
            return false;
        };
        let scope = self.impl_scope.clone();
        obligations
            .iter()
            .all(|(a, b)| self.obligation_holds(*a, b, &scope))
    }

    /// The bounds of extension `e` (of impl `i`) for the type args with which it fits `t`:
    /// each type arg with a bound it must meet.
    pub(super) fn ext_obligations(
        &mut self,
        i: u32,
        e: usize,
        t: TyId,
    ) -> Option<Vec<(TyId, Bound)>> {
        let imp = &self.impls[i as usize];
        let mut slots = vec![None; imp.generics as usize];
        if !self.match_ty(imp.ty, t, &mut slots) {
            return None;
        }
        let args: Vec<TyId> = slots.iter().map(|s| s.unwrap_or(self.ty.error)).collect();
        let bounds = self.extensions[e].generics.bounds.clone();
        let mut out = vec![];
        for (bs, &a) in bounds.iter().zip(&args) {
            for b in bs {
                let args = b.args.iter().map(|x| self.subst(*x, &args)).collect();
                out.push((
                    a,
                    Bound {
                        iface: b.iface,
                        args,
                    },
                ));
            }
        }
        Some(out)
    }

    /// Is the `extend … implements` impl `a` strictly more specific than impl `b`: its target
    /// an instance of `b`'s and not the reverse, or the same pattern with bounds where `b` has
    /// none (the rule for extension methods, body/expr/method.rs)?
    pub(crate) fn ext_more_specific(&mut self, a: u32, b: u32) -> bool {
        let (ta, tb) = (self.impls[a as usize].ty, self.impls[b as usize].ty);
        let (na, nb) = (
            self.impls[a as usize].generics as usize,
            self.impls[b as usize].generics as usize,
        );
        if !self.instance_of(ta, tb, nb) {
            return false;
        }
        if !self.instance_of(tb, ta, na) {
            return true;
        }
        self.ext_bounded(a) && !self.ext_bounded(b)
    }

    /// Does the `extend` block of impl `i` bound any of its type parameters?
    pub(crate) fn ext_bounded(&self, i: u32) -> bool {
        self.ext_impls
            .get(&i)
            .is_some_and(|&e| crate::collect::ext_impls::has_bounds(self, e))
    }

    /// Is `t` an instance of `pattern` (with `n` type params)?
    fn instance_of(&mut self, t: TyId, pattern: TyId, n: usize) -> bool {
        let mut slots = vec![None; n];
        self.match_ty(pattern, t, &mut slots) && self.subst_known(pattern, &slots) == t
    }

    /// Can one type fit both `extend` impls `a` and `b` (their targets unify; bounds and
    /// interface arguments are not considered: a type implements an interface once)? `Some`
    /// with that type, or with `None` when it still names a type parameter of the blocks.
    pub(crate) fn ext_overlap(&mut self, a: u32, b: u32) -> Option<Option<TyId>> {
        let slots = self.ext_unify(a, b, None)?;
        let w = self.resolve_slots(self.impls[a as usize].ty, &slots);
        Some((!self.mentions_params(w)).then_some(w))
    }

    /// Unify `a`'s target (or `a_ty`, a type over `a`'s parameters) with `b`'s: the bindings of
    /// `a`'s parameters followed by `b`'s.
    fn ext_unify(&mut self, a: u32, b: u32, a_ty: Option<TyId>) -> Option<Vec<Option<TyId>>> {
        let (ia, ib) = (&self.impls[a as usize], &self.impls[b as usize]);
        let (na, nb) = (ia.generics, ib.generics);
        let (ta, tb) = (a_ty.unwrap_or(ia.ty), ib.ty);
        let tb = self.shifted(tb, na, nb);
        let mut slots = vec![None; (na + nb) as usize];
        self.unify(ta, tb, &mut slots, 0).then_some(slots)
    }

    /// `t`, over `n` type parameters, with them renumbered after `by` others.
    fn shifted(&mut self, t: TyId, by: u32, n: u32) -> TyId {
        let shift: Vec<TyId> = (0..n).map(|k| self.ty.param(by + k)).collect();
        self.subst(t, &shift)
    }

    /// `t` with the bound `slots` substituted (repeatedly: a binding can name other
    /// parameters); unbound parameters stay.
    fn resolve_slots(&mut self, t: TyId, slots: &[Option<TyId>]) -> TyId {
        let full: Vec<TyId> = (slots.iter().enumerate())
            .map(|(k, s)| s.unwrap_or_else(|| self.ty.param(k as u32)))
            .collect();
        let mut w = t;
        for _ in 0..8 {
            let next = self.subst(w, &full);
            if next == w {
                break;
            }
            w = next;
        }
        w
    }

    /// Do `extend` impls `a` and `b` implement their interface with the same arguments for
    /// every type both apply to (`slots`: a unification of their parameters, `a`'s first)?
    fn ext_args_agree(&mut self, a: u32, b: u32, slots: &[Option<TyId>]) -> bool {
        let (ia, ib) = (&self.impls[a as usize], &self.impls[b as usize]);
        let (na, nb) = (ia.generics, ib.generics);
        let (xs, ys) = (ia.iface_args.clone(), ib.iface_args.clone());
        xs.len() == ys.len()
            && xs.iter().zip(&ys).all(|(&x, &y)| {
                let y = self.shifted(y, na, nb);
                self.resolve_slots(x, slots) == self.resolve_slots(y, slots)
            })
    }

    /// An `extend` impl that a block with bounds, `i`, takes precedence over while giving the
    /// interface other arguments, together with whether it is a base class's (`i` fits a type,
    /// the other a base class of it). A type would then have one implementation or the other as
    /// the bounds of `i` hold or not, which is no answer when they depend on the other block.
    pub(crate) fn ext_shadowed(&mut self, i: u32, j: u32) -> Option<(u32, bool)> {
        for (h, k) in [(i, j), (j, i)] {
            if !self.ext_bounded(h) {
                continue;
            }
            // `h` more specific than `k`, for a type both fit.
            if self.ext_more_specific(h, k) {
                if let Some(slots) = self.ext_unify(h, k, None) {
                    if !self.ext_args_agree(h, k, &slots) {
                        return Some((if h == i { j } else { i }, false));
                    }
                }
            }
            // `h` for a type, `k` for its base class.
            if self.ext_over_base(h, k) {
                return Some((if h == i { j } else { i }, true));
            }
        }
        None
    }

    /// Can `extend` impl `h` fit a type whose base class `k` fits, with other interface
    /// arguments?
    fn ext_over_base(&mut self, h: u32, k: u32) -> bool {
        let th = self.impls[h as usize].ty;
        if matches!(self.ty.kind(th), TyKind::Param(_)) {
            // A blanket block fits every subclass of a class `k` is for.
            let tk = self.impls[k as usize].ty;
            if self.class_of(tk).is_none() {
                return false;
            }
            let n = self.impls[h as usize].generics + self.impls[k as usize].generics;
            return !self.ext_args_agree(h, k, &vec![None; n as usize]);
        }
        let mut cur = self.base_of(th);
        for _ in 0..64 {
            let Some(b) = cur else { break };
            if let Some(slots) = self.ext_unify(h, k, Some(b)) {
                if !self.ext_args_agree(h, k, &slots) {
                    return true;
                }
            }
            cur = self.base_of(b);
        }
        false
    }

    /// Unify `x` and `y`, binding the parameters `slots` stands for. Conservative: a type it
    /// cannot take apart (a deferred type operator) unifies with anything.
    fn unify(&mut self, x: TyId, y: TyId, slots: &mut [Option<TyId>], depth: u32) -> bool {
        if depth > 32 || x == y {
            return true;
        }
        let (kx, ky) = (self.ty.kind(x).clone(), self.ty.kind(y).clone());
        let all = |cx: &mut Self, xs: &[TyId], ys: &[TyId], slots: &mut [Option<TyId>]| {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(a, b)| cx.unify(*a, *b, slots, depth + 1))
        };
        match (kx, ky) {
            (TyKind::Param(i), _) if (i as usize) < slots.len() => match slots[i as usize] {
                Some(b) => self.unify(b, y, slots, depth + 1),
                None => {
                    slots[i as usize] = Some(y);
                    true
                }
            },
            (_, TyKind::Param(j)) if (j as usize) < slots.len() => {
                self.unify(y, x, slots, depth + 1)
            }
            (TyKind::Op(..), _) | (_, TyKind::Op(..)) => true,
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) | (TyKind::Dyn(d, xs), TyKind::Dyn(e, ys)) => {
                d == e && all(self, &xs, &ys, slots)
            }
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => all(self, &xs, &ys, slots),
            (TyKind::Array(a), TyKind::Array(b)) | (TyKind::Shared(a), TyKind::Shared(b)) => {
                self.unify(a, b, slots, depth + 1)
            }
            (TyKind::Option(a, n), TyKind::Option(b, m)) => {
                n == m && self.unify(a, b, slots, depth + 1)
            }
            (TyKind::Map(a, b), TyKind::Map(c, d))
            | (TyKind::Result(a, b), TyKind::Result(c, d))
            | (TyKind::Promise(a, b), TyKind::Promise(c, d)) => all(self, &[a, b], &[c, d], slots),
            (
                TyKind::FnPtr {
                    params: p,
                    ret: r,
                    throws: t,
                    rest,
                },
                TyKind::FnPtr {
                    params: q,
                    ret: s,
                    throws: u,
                    rest: rest2,
                },
            ) => rest == rest2 && all(self, &p, &q, slots) && all(self, &[r, t], &[s, u], slots),
            _ => false,
        }
    }

    /// The bounds type parameter `ty` has through blanket impls (`extend<T extends B> T
    /// implements I`) whose bounds it meets with its declared ones (`scope`) or with those it
    /// gets from other blanket impls (a chain `K` → `J` → `I`): `I` for each.
    pub fn blanket_bounds(&mut self, ty: TyId, scope: &[Vec<Bound>]) -> Vec<Bound> {
        if self.in_blanket || self.ext_impls.is_empty() {
            return vec![];
        }
        self.in_blanket = true;
        let mut blankets: Vec<(u32, usize)> = (self.ext_impls.iter())
            .map(|(&i, &e)| (i, e))
            .filter(|&(i, _)| {
                let imp = &self.impls[i as usize];
                imp.generics == 1 && matches!(self.ty.kind(imp.ty), TyKind::Param(0))
            })
            .collect();
        blankets.sort_unstable();
        // `scope` with what `ty` gets so far, so the next round sees it.
        let mut grown = scope.to_vec();
        let slot = match self.ty.kind(ty) {
            TyKind::Param(n) => Some(*n as usize),
            _ => None,
        };
        if let Some(n) = slot.filter(|&n| grown.len() <= n) {
            grown.resize(n + 1, vec![]);
        }
        // The blocks to look at again when `ty` gets a bound: those bounded by it or by an
        // interface it extends. A full pass ends the search, so a bound met another way is
        // found too; a chain of blocks takes one look per block, in any declaration order.
        let mut by_bound: FxHashMap<DefId, Vec<usize>> = FxHashMap::default();
        for (k, &(_, e)) in blankets.iter().enumerate() {
            for b in &self.extensions[e].generics.bounds[0] {
                by_bound.entry(b.iface).or_default().push(k);
            }
        }
        // What a type parameter meets: its bounds and the interfaces they extend (`satisfies`
        // for a `Param`, without scanning its bounds per question).
        let mut met: Option<FxHashSet<Bound>> = slot.map(|n| {
            let mut met = FxHashSet::default();
            for x in grown[n].clone() {
                met.extend(self.iface_ancestors(&x));
                met.insert(x);
            }
            met
        });
        let mut out: Vec<Bound> = vec![];
        let mut taken = vec![false; blankets.len()];
        let mut todo: Vec<usize> = (0..blankets.len()).rev().collect();
        let mut full = true;
        loop {
            while let Some(k) = todo.pop() {
                if taken[k] {
                    continue;
                }
                let (i, e) = blankets[k];
                let bounds = self.extensions[e].generics.bounds[0].clone();
                let ok = bounds.iter().all(|b| {
                    let b = Bound {
                        iface: b.iface,
                        args: b.args.iter().map(|x| self.subst(*x, &[ty])).collect(),
                    };
                    match &met {
                        Some(met) => met.contains(&b),
                        None => self.satisfies(ty, &b, &grown),
                    }
                });
                if !ok {
                    continue;
                }
                taken[k] = true;
                let imp = &self.impls[i as usize];
                let (iface, iargs) = (imp.iface, imp.iface_args.clone());
                let args = iargs.iter().map(|x| self.subst(*x, &[ty])).collect();
                let b = Bound { iface, args };
                if let Some(n) = slot {
                    let mut gets = vec![b.clone()];
                    gets.extend(self.iface_ancestors(&b));
                    for g in &gets {
                        todo.extend(by_bound.get(&g.iface).into_iter().flatten());
                    }
                    met.get_or_insert_with(FxHashSet::default).extend(gets);
                    grown[n].push(b.clone());
                    full = false;
                }
                out.push(b);
            }
            if full {
                break;
            }
            full = true;
            todo = (0..blankets.len()).filter(|&k| !taken[k]).rev().collect();
        }
        self.in_blanket = false;
        out
    }
}
