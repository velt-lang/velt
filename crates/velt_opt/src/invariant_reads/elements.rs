//! Stores into an array's scalar elements: the one kind of write a loop may make and still have
//! its header and object reads made in front of it.
//!
//! Such a store writes element memory, an array's data buffer of scalars, and no header or class
//! object lives in one: an array of arrays or of objects holds pointers to them, and an array of
//! aggregates is written through aggregate places, which are not stores of a scalar through an
//! element pointer. That is the argument behind the `!tbaa` tags of the LLVM backend
//! (velt_codegen_llvm `tbaa.rs`, whose "element" accesses are exactly these stores), so such a
//! store changes no read the pass moves: a header field, or a scalar field of a class object.
//!
//! An element pointer is found as the LLVM backend finds it (`tbaa::element_pointers`): a `Ptr`
//! local that is not a param, whose address is never taken, assigned only `ptradd` of a header's
//! data field (`elem_place` in velt_vir) or `ptradd` or a copy of another such local, as the
//! largest such set.

use velt_vir::vir::{
    AggId, AggLayout, Function, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

/// Per local of `func`: whether it only ever holds a pointer into an array's data buffer.
pub(super) fn element_pointers(aggs: &[AggLayout], func: &Function, array: AggId) -> Vec<bool> {
    let n = func.locals.len();
    let mut elem: Vec<bool> = (0..n)
        .map(|i| i >= func.params.len() && func.locals[i].ty == Ty::Ptr)
        .collect();
    let mut defs: Vec<(usize, &Rvalue)> = vec![];
    let mut assigned = vec![false; n];
    for block in &func.blocks {
        for stmt in &block.stmts {
            if let Stmt::Assign(place, rvalue) = stmt {
                if place.proj.is_empty() {
                    defs.push((place.local.0 as usize, rvalue));
                    assigned[place.local.0 as usize] = true;
                } else if place.proj.iter().all(|p| !matches!(p, Proj::Deref(_))) {
                    // A write into part of the local itself.
                    elem[place.local.0 as usize] = false;
                }
                if let Rvalue::AddrOf(p) = rvalue {
                    elem[p.local.0 as usize] = false;
                }
            }
        }
        if let Terminator::Call {
            dest: Some(place), ..
        } = &block.term
        {
            elem[place.local.0 as usize] = false;
        }
    }
    for (e, a) in elem.iter_mut().zip(&assigned) {
        *e &= *a;
    }
    loop {
        let mut changed = false;
        for &(local, rvalue) in &defs {
            if elem[local] && !from_element_pointer(aggs, func, array, &elem, rvalue) {
                elem[local] = false;
                changed = true;
            }
        }
        if !changed {
            return elem;
        }
    }
}

/// Whether `dest` is a store of a scalar through an element pointer.
pub(super) fn is_element_store(elem: &[bool], dest: &Place) -> bool {
    matches!(dest.proj.as_slice(), [Proj::Deref(t)] if t.is_scalar() && *t != Ty::V128)
        && elem.get(dest.local.0 as usize) == Some(&true)
}

/// Whether `rvalue` is a pointer into an array's data buffer, given the locals known so far.
fn from_element_pointer(
    aggs: &[AggLayout],
    func: &Function,
    array: AggId,
    elem: &[bool],
    rvalue: &Rvalue,
) -> bool {
    let base = match rvalue {
        Rvalue::Binary(velt_vir::vir::BinOp::PtrAdd, Operand::Copy(base), _) => base,
        Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => return elem[p.local.0 as usize],
        _ => return false,
    };
    if base.proj.is_empty() {
        return elem[base.local.0 as usize];
    }
    // Field 0 (`data`) of an array header.
    let Some((Proj::Field(0), before)) = base.proj.split_last() else {
        return false;
    };
    container(aggs, func, base, before) == Some(Ty::Agg(array))
}

/// The type of `place.local` projected by `proj`, if well-formed.
fn container(aggs: &[AggLayout], func: &Function, place: &Place, proj: &[Proj]) -> Option<Ty> {
    let mut ty = func.locals.get(place.local.0 as usize)?.ty;
    for p in proj {
        ty = match (p, ty) {
            (Proj::Deref(pointee), Ty::Ptr) => *pointee,
            (Proj::Cast(id), _) => Ty::Agg(*id),
            (Proj::Field(n), Ty::Agg(id)) => aggs.get(id.0 as usize)?.fields.get(*n as usize)?.0,
            _ => return None,
        };
    }
    Some(ty)
}
