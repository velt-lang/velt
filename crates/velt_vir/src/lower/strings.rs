//! Reading strings inline (rt_abi.md "Strings"): `s.length` and `s.charCodeAt(i)` need no
//! runtime call. A string is `{w0, w1, w2}`; the inline form has the top bit of `w2` set, its
//! byte length in bits 56..61 of `w2` and its bytes in the value itself; the static and heap
//! forms keep `{ptr, units << 32 | len}` in `w0`/`w1`. Both reads are branch-free selects on an
//! all-ones mask. `length` still counts bytes until #377 phase 2 switches it to code units.

use super::operand::proj;
use super::{cint, ice, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty, UnOp, STR_AGG};

/// `w1` of a static string holding `text`: its UTF-16 length in the high half and its byte
/// length in the low half.
pub(super) fn str_w1(text: &str) -> u64 {
    let Ok(len) = u32::try_from(text.len()) else {
        ice("a string literal longer than 4 GiB")
    };
    let units = text.encode_utf16().count() as u64;
    (units << 32) | len as u64
}

impl FnLower<'_, '_> {
    fn u64_op(&mut self, op: BinOp, a: Operand, b: Operand) -> Operand {
        self.rvalue_temp(Ty::U64, Rvalue::Binary(op, a, b))
    }

    /// `mask ? a : b` for a mask of all ones / all zeros.
    fn select(&mut self, mask: Operand, a: Operand, b: Operand) -> Operand {
        let not = self.rvalue_temp(Ty::U64, Rvalue::Unary(UnOp::BitNot, mask.clone()));
        let a = self.u64_op(BinOp::BitAnd, a, mask);
        let b = self.u64_op(BinOp::BitAnd, b, not);
        self.u64_op(BinOp::BitOr, a, b)
    }

    /// `(byte length, inline mask)` of the string at `p`.
    fn str_len_mask(&mut self, p: &Place) -> (Operand, Operand) {
        let word = |i| Operand::Copy(proj(p, Proj::Field(i)));
        let w1 = self.rvalue_temp(Ty::U64, Rvalue::Use(word(1)));
        let w2 = self.rvalue_temp(Ty::U64, Rvalue::Use(word(2)));
        let signed = self.rvalue_temp(Ty::I64, Rvalue::Cast(w2.clone(), Ty::I64));
        let fill = self.rvalue_temp(
            Ty::I64,
            Rvalue::Binary(BinOp::Shr, signed, cint(63, Ty::I64)),
        );
        let inline = self.rvalue_temp(Ty::U64, Rvalue::Cast(fill, Ty::U64));
        let top = self.u64_op(BinOp::UShr, w2, cint(56, Ty::U64));
        let short = self.u64_op(BinOp::BitAnd, top, cint(0x1f, Ty::U64));
        let long = self.u64_op(BinOp::BitAnd, w1, cint(0xffff_ffff, Ty::U64));
        let len = self.select(inline.clone(), short, long);
        (len, inline)
    }

    /// `s.length` (a `u64`).
    pub(super) fn str_len(&mut self, v: Operand) -> Operand {
        let p = self.operand_place(v, Ty::Agg(STR_AGG));
        self.str_len_mask(&p).0
    }

    /// `s.charCodeAt(i)`: the byte at `i` as an `i64`, or -1 when `i` is out of range.
    pub(super) fn str_char_code_at(&mut self, v: Operand, i: Operand) -> Operand {
        let p = self.operand_place(v, Ty::Agg(STR_AGG));
        let (len, inline) = self.str_len_mask(&p);
        let here = self.addr(p.clone());
        let here = self.rvalue_temp(Ty::U64, Rvalue::Cast(here, Ty::U64));
        let ptr = Operand::Copy(proj(&p, Proj::Field(0)));
        let data = self.select(inline, here, ptr);
        let at = self.rvalue_temp(Ty::U64, Rvalue::Cast(i, Ty::U64));
        let ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, at.clone(), len));
        let out = self.temp(Ty::I64);
        self.assign(Place::local(out), Rvalue::Use(cint(-1, Ty::I64)));
        let (load, join) = (self.new_block(), self.new_block());
        self.branch(ok, load, join);
        self.switch_to(load);
        let base = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(data, Ty::Ptr));
        let Operand::Copy(bp) = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, base, at))
        else {
            unreachable!("ICE: rvalue_temp returns a place")
        };
        let byte = Operand::Copy(proj(&bp, Proj::Deref(Ty::U8)));
        self.assign(Place::local(out), Rvalue::Cast(byte, Ty::I64));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(out))
    }
}
