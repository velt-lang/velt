//! The impls of `extend … implements I` blocks (collect/ext_impls.rs) in impl lookup.
//!
//! A type's own impls, and those of its base classes, come first (`Ctx::find_impl`); only a type
//! without one gets an `extend` block's: the most specific block whose target and bounds fit it,
//! looked for on the type and then on each base class. Collect refuses two blocks that can both
//! fit one type where neither is more specific ([`Ctx::ext_overlap`]), so that block is unique.
//!
//! A block's bounds are checked through impls again (`extend<T extends I> T implements J`), so
//! a lookup can come back to itself. A `(type, interface)` lookup that is in progress counts as
//! not fitting (`Ctx::impl_busy`), which ends every cycle; lowering does the same.

use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{DefId, TyId, TyKind};

/// How many `extend` lookups may nest (each checks a block's bounds through impls).
const MAX_NESTED: usize = 64;

impl Ctx<'_> {
    /// The most specific `extend … implements iface` impl that fits `ty`, or else its nearest
    /// base class (module docs).
    pub(super) fn find_ext_impl(
        &mut self,
        ty: TyId,
        iface: DefId,
    ) -> Option<(u32, Vec<TyId>, TyId)> {
        let cands = self.ext_by_iface.get(&iface).cloned()?;
        if self.impl_busy.len() >= MAX_NESTED || !self.impl_busy.insert((ty, iface)) {
            return None;
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
        self.impl_busy.remove(&(ty, iface));
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
        let imp = &self.impls[i as usize];
        let mut slots = vec![None; imp.generics as usize];
        if !self.match_ty(imp.ty, t, &mut slots) {
            return false;
        }
        let args: Vec<TyId> = slots.iter().map(|s| s.unwrap_or(self.ty.error)).collect();
        let bounds = self.extensions[e].generics.bounds.clone();
        let scope = self.impl_scope.clone();
        bounds.iter().zip(&args).all(|(bs, &a)| {
            bs.iter().all(|b| {
                let b = Bound {
                    iface: b.iface,
                    args: b.args.iter().map(|x| self.subst(*x, &args)).collect(),
                };
                self.satisfies(a, &b, &scope)
            })
        })
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

    /// Can one type fit both `extend` impls `a` and `b` (their targets and interface arguments
    /// unify; bounds are not considered)? `Some` with that type, or with `None` when it still
    /// names a type parameter of the blocks.
    pub(crate) fn ext_overlap(&mut self, a: u32, b: u32) -> Option<Option<TyId>> {
        let (ia, ib) = (&self.impls[a as usize], &self.impls[b as usize]);
        let (na, nb) = (ia.generics, ib.generics);
        let (ta, tb) = (ia.ty, ib.ty);
        let (aa, ab) = (ia.iface_args.clone(), ib.iface_args.clone());
        if aa.len() != ab.len() {
            return None;
        }
        // `b`'s parameters after `a`'s.
        let shift: Vec<TyId> = (0..nb).map(|k| self.ty.param(na + k)).collect();
        let tb = self.subst(tb, &shift);
        let mut slots = vec![None; (na + nb) as usize];
        if !self.unify(ta, tb, &mut slots, 0) {
            return None;
        }
        for (x, y) in aa.iter().zip(&ab) {
            let y = self.subst(*y, &shift);
            if !self.unify(*x, y, &mut slots, 0) {
                return None;
            }
        }
        let mut w = ta;
        for _ in 0..8 {
            w = self.subst_known(w, &slots);
        }
        Some((!self.mentions_params(w)).then_some(w))
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
    /// implements I`) whose bounds it meets with its declared ones (`scope`): `I` for each.
    pub fn blanket_bounds(&mut self, ty: TyId, scope: &[Vec<Bound>]) -> Vec<Bound> {
        if self.in_blanket || self.ext_impls.is_empty() {
            return vec![];
        }
        self.in_blanket = true;
        let mut blankets: Vec<(u32, usize)> =
            self.ext_impls.iter().map(|(&i, &e)| (i, e)).collect();
        blankets.sort_unstable();
        let mut out = vec![];
        for (i, e) in blankets {
            let imp = &self.impls[i as usize];
            if imp.generics != 1 || !matches!(self.ty.kind(imp.ty), TyKind::Param(0)) {
                continue;
            }
            let (iface, iargs) = (imp.iface, imp.iface_args.clone());
            let bounds = self.extensions[e].generics.bounds[0].clone();
            let ok = bounds.iter().all(|b| {
                let b = Bound {
                    iface: b.iface,
                    args: b.args.iter().map(|x| self.subst(*x, &[ty])).collect(),
                };
                self.satisfies(ty, &b, scope)
            });
            if ok {
                let args = iargs.iter().map(|x| self.subst(*x, &[ty])).collect();
                out.push(Bound { iface, args });
            }
        }
        self.in_blanket = false;
        out
    }
}
