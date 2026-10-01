//! Per-local usage facts shared by the scalar passes, and place typing.
//!
//! A local is *register-like* when it is a scalar whose address is never taken: then the only
//! way to change it is a whole-local assignment (or a call destination) naming it, so
//! value-tracking passes (constant/copy propagation, dead stores) may reason about it exactly.
//! Address-taken locals can be written through any pointer, call or `MemCopy`; they are never
//! value-tracked and their stores are never removed.

use velt_vir::vir::{AggLayout, Function, Local, Place, Proj, Ty};

use crate::visit::{derefs, stmt_places, term_places, PlaceUse};

/// Usage facts for one local.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LocalUsage {
    /// Whole-local assignments (`_l = …` or call destination `_l`), plus one for params.
    pub defs: u32,
    /// Writes to part of the local (field of an aggregate).
    pub partial_defs: u32,
    /// Reads of the value (operands, or the pointer value for places that dereference it).
    pub reads: u32,
    /// Whether `AddrOf` exposes the local's memory.
    pub address_taken: bool,
}

/// Usage facts for all locals of a function.
pub(crate) struct Usage {
    /// Indexed by local.
    pub locals: Vec<LocalUsage>,
    tys: Vec<Ty>,
}

impl Usage {
    /// Scan the whole function.
    pub fn of(func: &Function) -> Usage {
        let mut locals = vec![LocalUsage::default(); func.locals.len()];
        for l in locals.iter_mut().take(func.params.len()) {
            l.defs = 1;
        }
        let mut record = |p: &Place, use_: PlaceUse| {
            let Some(u) = locals.get_mut(p.local.0 as usize) else {
                return;
            };
            match use_ {
                _ if derefs(p) => u.reads += 1,
                PlaceUse::Read => u.reads += 1,
                PlaceUse::AddrOf => u.address_taken = true,
                PlaceUse::Write if p.proj.is_empty() => u.defs += 1,
                PlaceUse::Write => u.partial_defs += 1,
            }
        };
        for block in &func.blocks {
            for s in &block.stmts {
                stmt_places(s, &mut record);
            }
            term_places(&block.term, &mut record);
        }
        Usage {
            locals,
            tys: func.locals.iter().map(|l| l.ty).collect(),
        }
    }

    /// Whether the local can be value-tracked (scalar, never address-taken).
    pub fn is_register(&self, l: Local) -> bool {
        let i = l.0 as usize;
        match (self.locals.get(i), self.tys.get(i)) {
            (Some(u), Some(ty)) => ty.is_scalar() && !u.address_taken,
            _ => false,
        }
    }

    /// Facts about one local.
    pub fn get(&self, l: Local) -> LocalUsage {
        self.locals.get(l.0 as usize).copied().unwrap_or_default()
    }
}

/// Type of a place, or `None` if a projection does not fit the type it is applied to.
pub(crate) fn place_ty(aggs: &[AggLayout], func: &Function, p: &Place) -> Option<Ty> {
    let mut ty = func.locals.get(p.local.0 as usize)?.ty;
    for proj in &p.proj {
        ty = match (proj, ty) {
            (Proj::Field(n), Ty::Agg(id)) => aggs.get(id.0 as usize)?.fields.get(*n as usize)?.0,
            (Proj::Deref(pointee), Ty::Ptr) => *pointee,
            (Proj::Cast(id), _) => Ty::Agg(*id),
            _ => return None,
        };
    }
    Some(ty)
}

/// Whether the place is exactly a register-like local (no projections).
pub(crate) fn as_register(usage: &Usage, p: &Place) -> Option<Local> {
    (p.proj.is_empty() && usage.is_register(p.local)).then_some(p.local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{Rvalue, Terminator};

    #[test]
    fn classifies_reads_writes_and_address_taken() {
        let mut fb = FuncBuilder::internal("f", &[Ty::Ptr], Ty::I64);
        let p = fb.param(0);
        let x = fb.local(Ty::I64);
        let y = fb.local(Ty::I64);
        let q = fb.local(Ty::Ptr);
        let b = fb.block();
        fb.assign(b, x, Rvalue::Use(copy_place(deref(p, Ty::I64))));
        fb.push(
            b,
            velt_vir::vir::Stmt::Assign(deref(p, Ty::I64), Rvalue::Use(int(1, Ty::I64))),
        );
        fb.assign(b, q, Rvalue::AddrOf(Place::local(y)));
        fb.assign(b, y, Rvalue::Use(copy_local(x)));
        fb.term(b, Terminator::Return(copy_local(y)));
        let f = fb.finish();
        let u = Usage::of(&f);
        assert_eq!(u.get(p).reads, 2);
        assert_eq!(u.get(p).defs, 1);
        assert!(u.is_register(x));
        assert!(!u.is_register(y));
        assert_eq!(u.get(y).defs, 1);
        assert_eq!(u.get(q).reads, 0);
    }
}
