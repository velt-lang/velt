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
pub(crate) fn element_pointers(aggs: &[AggLayout], func: &Function, array: AggId) -> Vec<bool> {
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
pub(crate) fn is_element_store(elem: &[bool], dest: &Place) -> bool {
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

#[cfg(test)]
mod tests {
    //! One function per rule of [`element_pointers`]: each local named below is a candidate that
    //! a rule must rule out (or, `elem`, the one that stays).

    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{BinOp, Callee, Const, Local, Program};

    /// `f(p: ptr)` with a header `h`: `elem = ptradd (*h).0, 8` (an element pointer), and the
    /// locals each rule is about. Returns the program and the locals by name.
    fn function() -> (Program, AggId, Vec<(&'static str, Local)>) {
        let mut pb = ProgramBuilder::new();
        let hdr = pb.agg(
            velt_vir::ARRAY_AGG_NAME,
            24,
            8,
            &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)],
        );
        let mut fb = FuncBuilder::internal("make", &[], Ty::Ptr);
        let b = fb.block();
        fb.ret(b, int(0, Ty::Ptr));
        let make = pb.add(fb.finish());

        let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::Unit);
        let p = fb.param(0);
        let (hv, h) = (fb.local(Ty::Agg(hdr)), fb.local(Ty::Ptr));
        let locals: Vec<(&'static str, Local)> = [
            "elem",
            "len_field",
            "cap_field",
            "address_taken",
            "call_dest",
            "from_param",
        ]
        .iter()
        .map(|&n| (n, fb.local(Ty::Ptr)))
        .collect();
        let get = |n: &str| locals.iter().find(|(m, _)| *m == n).expect("named").1;
        let (entry, after) = (fb.block(), fb.block());
        fb.assign(entry, h, Rvalue::AddrOf(Place::local(hv)));
        let field = |n: u32| Place {
            local: h,
            proj: vec![Proj::Deref(Ty::Agg(hdr)), Proj::Field(n)],
        };
        let off = || int(8, Ty::U64);
        fb.assign(
            entry,
            get("elem"),
            bin(BinOp::PtrAdd, copy_place(field(0)), off()),
        );
        // A param: the caller's pointer, which may point anywhere.
        fb.assign(entry, p, bin(BinOp::PtrAdd, copy_place(field(0)), off()));
        // Fields of the header other than its data pointer.
        fb.assign(
            entry,
            get("len_field"),
            bin(BinOp::PtrAdd, copy_place(field(1)), off()),
        );
        fb.assign(
            entry,
            get("cap_field"),
            bin(BinOp::PtrAdd, copy_place(field(2)), off()),
        );
        // A local whose address is taken: a store through that address may change it.
        let at = get("address_taken");
        fb.assign(entry, at, bin(BinOp::PtrAdd, copy_place(field(0)), off()));
        let addr = fb.local(Ty::Ptr);
        fb.assign(entry, addr, Rvalue::AddrOf(Place::local(at)));
        // A copy of the param.
        fb.assign(
            entry,
            get("from_param"),
            bin(BinOp::PtrAdd, copy_local(p), off()),
        );
        // A call's destination, also assigned an element pointer elsewhere.
        let cd = get("call_dest");
        fb.assign(entry, cd, bin(BinOp::PtrAdd, copy_place(field(0)), off()));
        let next = fb.call(entry, Callee::Func(make), vec![], Some(cd));
        fb.goto(next, after);
        fb.ret(after, Operand::Const(Const::Unit, Ty::Unit));
        let mut locals = locals;
        locals.push(("param", p));
        pb.add(fb.finish());
        (pb.finish(), hdr, locals)
    }

    #[test]
    fn only_a_ptradd_of_a_header_s_data_is_an_element_pointer() {
        let (p, hdr, locals) = function();
        let f = p.funcs.last().expect("f");
        let elem = element_pointers(&p.aggs, f, hdr);
        for (name, l) in locals {
            let want = name == "elem";
            assert_eq!(elem[l.0 as usize], want, "{name}");
        }
    }
}
