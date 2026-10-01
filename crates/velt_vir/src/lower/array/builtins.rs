//! Generated functions shared by all arrays: buffer growth (`ArrayGrow`) and the bounds-check
//! panic (`Oob`), which formats `index out of bounds: the len is L but the index is I`.

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, Cx, FnLower, ScopeKind};
use crate::vir::{self, BinOp, Function, Operand, Place, Proj, Rvalue, Ty};

impl<'c, 'h> FnLower<'c, 'h> {
    /// `(arr: ptr, stride: u64, align: u64)`: double the capacity (4 when empty).
    pub(in crate::lower) fn build_array_grow(cx: &'c mut Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let a = lw.cx.array_agg();
        let arr_p = lw.new_local(Ty::Ptr, Some("arr".into()));
        let stride = Operand::Copy(Place::local(lw.new_local(Ty::U64, Some("stride".into()))));
        let align = Operand::Copy(Place::local(lw.new_local(Ty::U64, Some("align".into()))));
        let arr = proj(&Place::local(arr_p), Proj::Deref(Ty::Agg(a)));
        let cap = lw.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(&arr, 2)));
        let empty = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, cap.clone(), cint(0, Ty::U64)),
        );
        let (new_bb, grow_bb, join) = (lw.new_block(), lw.new_block(), lw.new_block());
        let newcap = lw.temp(Ty::U64);
        let data = lw.temp(Ty::Ptr);
        lw.branch(empty, new_bb, grow_bb);
        lw.switch_to(new_bb);
        lw.assign(Place::local(newcap), Rvalue::Use(cint(4, Ty::U64)));
        let size = lw.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, cint(4, Ty::U64), stride.clone()),
        );
        lw.call_rt(
            Rt::Alloc,
            vec![size, align.clone()],
            Some(Place::local(data)),
        );
        lw.goto(join);
        lw.switch_to(grow_bb);
        let dbl = lw.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, cap.clone(), cint(2, Ty::U64)),
        );
        lw.assign(Place::local(newcap), Rvalue::Use(dbl.clone()));
        let old = lw.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Mul, cap, stride.clone()));
        let new = lw.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Mul, dbl, stride));
        let args = vec![Self::arr_field(&arr, 0), old, align, new];
        lw.call_rt(Rt::Realloc, args, Some(Place::local(data)));
        lw.goto(join);
        lw.switch_to(join);
        lw.assign(
            proj(&arr, Proj::Field(0)),
            Rvalue::Use(Operand::Copy(Place::local(data))),
        );
        lw.assign(
            proj(&arr, Proj::Field(2)),
            Rvalue::Use(Operand::Copy(Place::local(newcap))),
        );
        lw.terminate(vir::Terminator::Return(unit()));
        lw.finish(
            "_Garray_grow".into(),
            vec![Ty::Ptr, Ty::U64, Ty::U64],
            Ty::Unit,
        )
    }

    /// `(len: u64, index, at: ptr)`: `panic("index out of bounds: the len is {len} but the index
    /// is {i}{*at}")` (`*at` is the ` at <location>` suffix, possibly empty).
    pub(in crate::lower) fn build_oob(cx: &'c mut Cx<'h>, signed: bool) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let ity = if signed { Ty::I64 } else { Ty::U64 };
        let len = lw.new_local(Ty::U64, Some("len".into()));
        let idx = lw.new_local(ity, Some("index".into()));
        let at = lw.new_local(Ty::Ptr, Some("at".into()));
        lw.push_scope(ScopeKind::Block);
        let str_ty = lw.cx.str_ty();
        let mut parts = vec![];
        for (text, num, rt) in [
            ("index out of bounds: the len is ", len, Rt::StrFromU64),
            (
                " but the index is ",
                idx,
                if signed {
                    Rt::StrFromI64
                } else {
                    Rt::StrFromU64
                },
            ),
        ] {
            parts.push(lw.str_lit(text));
            let s = lw.temp(Ty::Agg(crate::vir::STR_AGG));
            let o = lw.addr(Place::local(s));
            lw.call_rt(rt, vec![Operand::Copy(Place::local(num)), o], None);
            parts.push(Operand::Copy(Place::local(s)));
        }
        let str_vt = Ty::Agg(crate::vir::STR_AGG);
        parts.push(Operand::Copy(proj(&Place::local(at), Proj::Deref(str_vt))));
        let mut acc = parts[0].clone();
        for p in parts.into_iter().skip(1) {
            let a = lw.operand_addr(acc, str_vt);
            let b = lw.operand_addr(p, str_vt);
            acc = lw.concat(a, b, str_ty);
        }
        let msg = lw.operand_addr(acc, str_vt);
        lw.call_rt(Rt::Panic, vec![msg], None);
        lw.scopes.clear();
        let sym = if signed { "_Goob_i64" } else { "_Goob_u64" };
        lw.finish(sym.into(), vec![Ty::U64, ity, Ty::Ptr], Ty::Unit)
    }
}
