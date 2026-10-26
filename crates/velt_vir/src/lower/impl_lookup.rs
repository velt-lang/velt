//! The impl of an interface for a concrete type (`Program::impls`), as sema's `Ctx::find_impl`
//! picks it: the type's own impl or that of its nearest base class implementing the interface,
//! the first in `impls` order; else the most specific `extend … implements` block's impl that
//! fits the type (its bounds holding, `Program::extension_impls`), or else a base class.
//!
//! A block's bounds are checked through impls again, so a lookup can come back to itself: one
//! that is in progress counts as not fitting (`Cx::impl_busy`), as in sema.

use velt_sema::hir::{DefId, TyId};

use super::ice;

/// How many `extend` lookups may nest.
const MAX_NESTED: usize = 64;

/// An impl that fits a type: its index, the type it is for (the type or a base class) and the
/// bindings of its type parameters.
pub(super) struct Fit {
    pub index: u32,
    pub ty: TyId,
    pub binds: Vec<Option<TyId>>,
}

impl super::Cx<'_> {
    /// The impl of `iface<iargs>` for concrete `ty` (or a base class of it): its index.
    pub(super) fn find_impl(&mut self, iface: DefId, iargs: &[TyId], ty: TyId) -> u32 {
        match self.impl_for(iface, Some(iargs), ty) {
            Some(f) => f.index,
            None => ice(format_args!(
                "no impl of interface for type `{}`",
                self.type_name(ty)
            )),
        }
    }

    /// The impl of `iface` (with arguments `iargs`, or any) for `ty` (module docs).
    fn impl_for(&mut self, iface: DefId, iargs: Option<&[TyId]>, ty: TyId) -> Option<Fit> {
        let impls = self.impls_of(iface);
        let ext = !self.hir.extension_impls.is_empty();
        for cand in self.self_and_bases(ty) {
            for &i in impls.iter() {
                if ext && self.hir.extension_impls.contains_key(&i) {
                    continue;
                }
                if let Some(binds) = self.impl_fits(i, iargs, cand) {
                    return Some(Fit {
                        index: i,
                        ty: cand,
                        binds,
                    });
                }
            }
        }
        if ext {
            return self.ext_impl_for(iface, iargs, ty);
        }
        None
    }

    /// The most specific `extend … implements iface` impl that fits `ty`, or else its nearest
    /// base class.
    pub(super) fn ext_impl_for(
        &mut self,
        iface: DefId,
        iargs: Option<&[TyId]>,
        ty: TyId,
    ) -> Option<Fit> {
        if self.impl_busy.len() >= MAX_NESTED || !self.impl_busy.insert((iface, ty)) {
            return None;
        }
        let impls = self.impls_of(iface);
        let mut best: Option<Fit> = None;
        for cand in self.self_and_bases(ty) {
            for &i in impls.iter() {
                if !self.hir.extension_impls.contains_key(&i) {
                    continue;
                }
                // Two blocks that both fit are ordered (sema refuses others), so one that is
                // not more specific than the best so far is not the most specific.
                if best
                    .as_ref()
                    .is_some_and(|b| !self.ext_more_specific(i, b.index))
                {
                    continue;
                }
                if let Some(binds) = self.impl_fits(i, iargs, cand) {
                    best = Some(Fit {
                        index: i,
                        ty: cand,
                        binds,
                    });
                }
            }
            if best.is_some() {
                break;
            }
        }
        self.impl_busy.remove(&(iface, ty));
        best
    }

    /// The bindings with which impl `i` implements `<iargs>` (or any arguments) for `ty`, its
    /// bounds (an `extend` block's) holding.
    fn impl_fits(&mut self, i: u32, iargs: Option<&[TyId]>, ty: TyId) -> Option<Vec<Option<TyId>>> {
        let imp = &self.hir.impls[i as usize];
        let mut binds = vec![None; imp.generics as usize];
        let fits = self.match_ty(imp.ty, ty, &mut binds)
            && iargs.is_none_or(|a| {
                imp.iface_args.len() == a.len() && self.match_all(&imp.iface_args, a, &mut binds)
            });
        if !fits {
            return None;
        }
        let Some(bounds) = self.hir.extension_impls.get(&i).cloned() else {
            return Some(binds);
        };
        let args: Vec<TyId> = binds
            .iter()
            .map(|b| b.unwrap_or_else(|| ice("impl type parameter left unbound")))
            .collect();
        let holds = bounds.iter().all(|b| {
            let t = args[b.param as usize];
            let bargs: Vec<TyId> = b.args.iter().map(|&a| self.subst(a, &args)).collect();
            self.impl_for(b.iface, Some(&bargs), t).is_some()
        });
        holds.then_some(binds)
    }

    /// Is `extend` impl `a` strictly more specific than impl `b`: its target an instance of
    /// `b`'s and not the reverse, or the same pattern where only `a` has bounds (sema's
    /// `Ctx::ext_more_specific`)?
    fn ext_more_specific(&self, a: u32, b: u32) -> bool {
        let (pa, pb) = (&self.hir.impls[a as usize], &self.hir.impls[b as usize]);
        let instance = |t: TyId, pat: TyId, n: u32| {
            let mut binds = vec![None; n as usize];
            self.match_ty(pat, t, &mut binds)
        };
        if !instance(pa.ty, pb.ty, pb.generics) {
            return false;
        }
        if !instance(pb.ty, pa.ty, pa.generics) {
            return true;
        }
        let bounded = |i: u32| {
            self.hir
                .extension_impls
                .get(&i)
                .is_some_and(|bs| !bs.is_empty())
        };
        bounded(a) && !bounded(b)
    }
}
