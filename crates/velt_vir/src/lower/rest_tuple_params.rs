//! A closure whose rest parameter is a tuple at an instantiation (`(rq, ...args) => …` typed by
//! `(rq: Ctx, ...args: A) => R` at `A = [f64, string]`, #379, #943). Its function type there
//! has the tuple's elements as fixed parameters (`fn_rest_tuple.rs`), and callers pass them so.
//! The closure's code takes them the same way, one parameter each (Copy values by value, others
//! by pointer, borrowed, as for any parameter of a function value), and builds the rest local,
//! the tuple, from them on entry: each element shared, the tuple owned by the frame. Its thunk
//! (glue/thunk.rs) passes them on as they come.

use velt_sema::hir::{self, DefId, FnDef, TyId, TyKind};

use super::operand::proj;
use super::{FnLower, LInfo, LState};
use crate::vir::{Local, Operand, ParamAttrs, Place, Proj, Rvalue, Ty};

/// A rest tuple's parameters, declared, whose tuple is built once every parameter is.
pub(super) struct RestTuple {
    local: usize,
    ty: TyId,
    name: String,
    /// Each element's parameter, its type, and whether it is passed by pointer.
    elems: Vec<(Local, TyId, bool)>,
}

impl FnLower<'_, '_> {
    /// The elements of the rest parameter `p` of closure `def` (the last of `f`'s), when it is a
    /// tuple at this instance and the closure was made with a rest parameter; else `None`.
    pub(super) fn rest_tuple_of(
        &mut self,
        def: DefId,
        f: &FnDef,
        p: &hir::Param,
    ) -> Option<Vec<TyId>> {
        if !self.cx.rest_closures.contains(&def)
            || f.params.last().map(|l| l.local) != Some(p.local)
        {
            return None;
        }
        let ty = self.sub(p.ty);
        match self.cx.kind(ty) {
            TyKind::Tuple(ts) => Some(ts),
            _ => None,
        }
    }

    /// Declares the elements `ts` of rest parameter `p` (named `name`) as parameters; the
    /// tuple is built by [`Self::finish_rest_tuple`] once every parameter is declared.
    pub(super) fn rest_tuple_params(
        &mut self,
        p: &hir::Param,
        ts: &[TyId],
        name: &str,
        params: &mut Vec<Ty>,
        attrs: &mut Vec<ParamAttrs>,
    ) -> RestTuple {
        let mut elems = vec![];
        for (i, &et) in ts.iter().enumerate() {
            let evt = self.cx.ty(et);
            // A `void` element is passed as nothing and has no field (`stored_tys`).
            if evt == Ty::Unit {
                continue;
            }
            let indirect = matches!(evt, Ty::Agg(_));
            let pt = if indirect { Ty::Ptr } else { evt };
            params.push(pt);
            attrs.push(ParamAttrs::default());
            let l = self.new_local(pt, Some(format!("{name}#{i}")));
            elems.push((l, et, indirect));
        }
        RestTuple {
            local: p.local.0 as usize,
            ty: self.sub(p.ty),
            name: name.to_string(),
            elems,
        }
    }

    /// The rest local of `r`, built on entry from its elements' parameters (module docs).
    pub(super) fn finish_rest_tuple(&mut self, r: RestTuple, info: &mut [Option<LInfo>]) {
        let (local, ty) = (r.local, r.ty);
        let t = self.build_rest_tuple(r);
        let droppable = t.is_some() && self.cx.needs_drop(ty);
        info[local] = Some(LInfo::new(t, ty, false, droppable, LState::Init));
    }

    /// A new local holding the tuple of `r`'s elements, each shared; `None` for a tuple with
    /// no stored field (`[]`, `[void]`).
    pub(super) fn build_rest_tuple(&mut self, r: RestTuple) -> Option<Local> {
        let vt = self.cx.ty(r.ty);
        let Ty::Agg(a) = vt else {
            return None;
        };
        let mut vals = vec![];
        for (l, et, indirect) in r.elems {
            let at = match indirect {
                true => proj(&Place::local(l), Proj::Deref(self.cx.ty(et))),
                false => Place::local(l),
            };
            vals.push(self.share_value(Operand::Copy(at), et));
        }
        let t = self.new_local(vt, Some(r.name));
        self.assign(Place::local(t), Rvalue::Aggregate(a, vals));
        Some(t)
    }
}
