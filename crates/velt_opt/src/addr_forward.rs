//! Pointer forwarding. A pointer local assigned exactly once, from the address of a fixed
//! location (a local, possibly with field / cast projections, but no dereference), holds that
//! address wherever it is read. Places that dereference it are rewritten to name the location
//! directly: with `p = &a.1`, `(*p as T).0` becomes `a.1.0` (when `a.1` has type `T`).
//!
//! Lowering passes aggregates by pointer (`this`, borrowed args, out-pointers), so after
//! inlining most struct accesses go through such pointers. Once forwarded, the pointer is
//! usually dead (dce removes it), the location may stop being address-taken, and the
//! value-tracking passes can see scalar locals that were only reachable through memory.

use std::collections::HashMap;

use velt_vir::vir::{AggLayout, Function, Local, Place, Proj, Rvalue, Stmt, Ty};

use crate::locals::{place_ty, Usage};
use crate::visit::{derefs, places_mut};

/// Forward single-definition address locals in `func`; returns whether any place changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let targets = targets(aggs, func);
    if targets.is_empty() {
        return false;
    }
    let mut changed = false;
    places_mut(func, &mut |place| {
        let Some((target, target_ty)) = targets.get(&place.local) else {
            return;
        };
        if place.proj.first() != Some(&Proj::Deref(*target_ty)) {
            return;
        }
        let mut proj = target.proj.clone();
        proj.extend(place.proj.drain(1..));
        *place = Place {
            local: target.local,
            proj,
        };
        changed = true;
    });
    changed
}

/// Pointer local → (the location it always points to, that location's type).
fn targets(aggs: &[AggLayout], func: &Function) -> HashMap<Local, (Place, Ty)> {
    let usage = Usage::of(func);
    let mut out = HashMap::new();
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        let Stmt::Assign(dst, Rvalue::AddrOf(target)) = s else {
            continue;
        };
        let p = dst.local;
        let single = dst.proj.is_empty()
            && (p.0 as usize) >= func.params.len()
            && usage.is_register(p)
            && usage.get(p).defs == 1;
        if !single || derefs(target) || target.local == p {
            continue;
        }
        if let Some(ty) = place_ty(aggs, func, target) {
            out.insert(p, (target.clone(), ty));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{Operand, Terminator};

    #[test]
    fn forwards_field_address_and_leaves_reassigned_pointers() {
        // p = &a.1; (*p as pair).0 = 7; x = (*p as pair).1;
        // q = &a; q = &b; y = (*q as outer).0   — q has two defs: untouched.
        let mut pb = ProgramBuilder::new();
        let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
        let outer = pb.agg("outer", 24, 8, &[(Ty::I64, 0), (Ty::Agg(pair), 8)]);
        let mut fb = FuncBuilder::export("f", &[], Ty::I64);
        let (a, b_, p, q, x, y) = (
            fb.local(Ty::Agg(outer)),
            fb.local(Ty::Agg(outer)),
            fb.local(Ty::Ptr),
            fb.local(Ty::Ptr),
            fb.local(Ty::I64),
            fb.local(Ty::I64),
        );
        let b = fb.block();
        let through = |l, agg, n| Place {
            local: l,
            proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(n)],
        };
        fb.assign(b, p, Rvalue::AddrOf(field(a, 1)));
        fb.push(
            b,
            Stmt::Assign(through(p, pair, 0), Rvalue::Use(int(7, Ty::I64))),
        );
        fb.assign(b, x, Rvalue::Use(copy_place(through(p, pair, 1))));
        fb.assign(b, q, Rvalue::AddrOf(Place::local(a)));
        fb.assign(b, q, Rvalue::AddrOf(Place::local(b_)));
        fb.assign(b, y, Rvalue::Use(copy_place(through(q, outer, 0))));
        fb.term(b, Terminator::Return(copy_local(x)));
        pb.add(fb.finish());
        let mut program = pb.finish();
        let aggs = program.aggs.clone();
        assert!(run(&aggs, &mut program.funcs[0]));
        let stmts = &program.funcs[0].blocks[0].stmts;
        let a_1_0 = Place {
            local: a,
            proj: vec![Proj::Field(1), Proj::Field(0)],
        };
        assert_eq!(stmts[1], Stmt::Assign(a_1_0, Rvalue::Use(int(7, Ty::I64))));
        let a_1_1 = Place {
            local: a,
            proj: vec![Proj::Field(1), Proj::Field(1)],
        };
        assert_eq!(
            stmts[2],
            Stmt::Assign(Place::local(x), Rvalue::Use(Operand::Copy(a_1_1)))
        );
        assert_eq!(
            stmts[5],
            Stmt::Assign(
                Place::local(y),
                Rvalue::Use(copy_place(through(q, outer, 0)))
            )
        );
    }
}
