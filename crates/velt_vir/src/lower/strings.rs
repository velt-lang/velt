//! Reading strings inline (rt_abi.md "Strings"): `s.length` and `s.charCodeAt(i)` of an ASCII
//! string need no runtime call. A string is `{w0, w1, w2}`; the inline form has the top bit of
//! `w2` set, its byte length in bits 56..61 of `w2` (byte 23), bit 62 set when it is not ASCII,
//! whose unit count is then byte 22, and its bytes in the value itself; the static and heap forms
//! keep `{ptr, units << 32 | len}` in `w0`/`w1`. Both reads are branch-free selects on all-ones
//! masks. `length` counts UTF-16 code units (#377 phase 2b); a string is ASCII exactly when its
//! unit count equals its byte count, and only then does `charCodeAt` read a byte.

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty, UnOp, STR_AGG};

/// `w1` of a static string holding `text`: its UTF-16 length in the high half and its byte
/// length in the low half.
pub(super) fn str_w1(text: &str) -> u64 {
    let Ok(len) = i32::try_from(text.len()) else {
        ice("a string literal of 2 GiB or more")
    };
    let units = text.encode_utf16().count() as u64;
    (units << 32) | len as u64
}

impl FnLower<'_, '_> {
    fn u64_op(&mut self, op: BinOp, a: Operand, b: Operand) -> Operand {
        self.rvalue_temp(Ty::U64, Rvalue::Binary(op, a, b))
    }

    /// `mask ? a : b` for a mask of all ones / all zeros.
    pub(super) fn select(&mut self, mask: Operand, a: Operand, b: Operand) -> Operand {
        let not = self.rvalue_temp(Ty::U64, Rvalue::Unary(UnOp::BitNot, mask.clone()));
        let a = self.u64_op(BinOp::BitAnd, a, mask);
        let b = self.u64_op(BinOp::BitAnd, b, not);
        self.u64_op(BinOp::BitOr, a, b)
    }

    /// The words of the string at `p` that the reads need: `(w1, w2, inline mask, byte 23)`.
    fn str_words(&mut self, p: &Place) -> (Operand, Operand, Operand, Operand) {
        let word = |i| Operand::Copy(proj(p, Proj::Field(i)));
        let w1 = self.rvalue_temp(Ty::U64, Rvalue::Use(word(1)));
        let w2 = self.rvalue_temp(Ty::U64, Rvalue::Use(word(2)));
        let inline = self.sign_mask(w2.clone(), 0);
        let top = self.u64_op(BinOp::UShr, w2.clone(), cint(56, Ty::U64));
        (w1, w2, inline, top)
    }

    /// All ones when bit `63 - bit` of `w` is set, else zero.
    fn sign_mask(&mut self, w: Operand, bit: u64) -> Operand {
        let w = if bit == 0 {
            w
        } else {
            self.u64_op(BinOp::Shl, w, cint(bit as i128, Ty::U64))
        };
        let signed = self.rvalue_temp(Ty::I64, Rvalue::Cast(w, Ty::I64));
        let fill = self.rvalue_temp(
            Ty::I64,
            Rvalue::Binary(BinOp::Shr, signed, cint(63, Ty::I64)),
        );
        self.rvalue_temp(Ty::U64, Rvalue::Cast(fill, Ty::U64))
    }

    /// The byte length, from [`Self::str_words`].
    fn str_bytes(&mut self, w1: Operand, inline: Operand, top: Operand) -> Operand {
        let short = self.u64_op(BinOp::BitAnd, top, cint(0x1f, Ty::U64));
        // The low half of `w1`, sign-extended: strings are shorter than 2 GiB, so this is the byte
        // length. Zero-extending it (`w1 & 0xffff_ffff`) would tell LLVM the length fits in 32
        // bits, which lets it vectorize `charCodeAt` scan loops into slower SSE2 code (#377 phase
        // 1 measured bench/strings 16% slower).
        let low = self.rvalue_temp(Ty::I32, Rvalue::Cast(w1, Ty::I32));
        let low = self.rvalue_temp(Ty::I64, Rvalue::Cast(low, Ty::I64));
        let long = self.rvalue_temp(Ty::U64, Rvalue::Cast(low, Ty::U64));
        self.select(inline, short, long)
    }

    /// The UTF-16 length, from [`Self::str_words`]: byte 23's length for an inline ASCII string,
    /// byte 22 for an inline non-ASCII one, the high half of `w1` otherwise.
    fn str_units(&mut self, w1: Operand, w2: Operand, inline: Operand, top: Operand) -> Operand {
        let short = self.u64_op(BinOp::BitAnd, top, cint(0x1f, Ty::U64));
        // Bit 0x40 of byte 23 (bit 62 of `w2`): an inline string that is not ASCII. Clear for the
        // other forms (a capacity is below 2 GiB, a static string's `w2` is 0).
        let non_ascii = self.sign_mask(w2.clone(), 1);
        let byte22 = self.u64_op(BinOp::UShr, w2, cint(48, Ty::U64));
        let byte22 = self.u64_op(BinOp::BitAnd, byte22, cint(0xff, Ty::U64));
        let inline_units = self.select(non_ascii, byte22, short);
        // The high half, by an arithmetic shift: like the byte length, it reads as a signed 32-bit
        // number (exact below 2 GiB), so LLVM sees the same range for both (see `str_bytes`).
        let signed = self.rvalue_temp(Ty::I64, Rvalue::Cast(w1, Ty::I64));
        let high = self.rvalue_temp(
            Ty::I64,
            Rvalue::Binary(BinOp::Shr, signed, cint(32, Ty::I64)),
        );
        let long = self.rvalue_temp(Ty::U64, Rvalue::Cast(high, Ty::U64));
        self.select(inline, inline_units, long)
    }

    /// `s.length` (a `u64`): UTF-16 code units.
    pub(super) fn str_len(&mut self, v: Operand) -> Operand {
        let p = self.operand_place(v, Ty::Agg(STR_AGG));
        let (w1, w2, inline, top) = self.str_words(&p);
        self.str_units(w1, w2, inline, top)
    }

    /// `s.charCodeAt(i)`: the UTF-16 code unit at `i` as an `i64`, or -1 when `i` is out of range.
    /// An ASCII string (units == bytes) loads the byte inline, as before #377 phase 2b; any other
    /// string calls the runtime, which translates the position.
    pub(super) fn str_char_code_at(&mut self, v: Operand, i: Operand) -> Operand {
        let p = self.operand_place(v, Ty::Agg(STR_AGG));
        let (w1, w2, inline, top) = self.str_words(&p);
        let len = self.str_bytes(w1.clone(), inline.clone(), top.clone());
        let units = self.str_units(w1, w2, inline.clone(), top);
        let ascii = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, units, len.clone()));
        let here = self.addr(p.clone());
        let here = self.rvalue_temp(Ty::U64, Rvalue::Cast(here, Ty::U64));
        let ptr = Operand::Copy(proj(&p, Proj::Field(0)));
        let data = self.select(inline, here, ptr);
        let at = self.rvalue_temp(Ty::U64, Rvalue::Cast(i.clone(), Ty::U64));
        let in_range = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, at.clone(), len));
        let ok = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::BitAnd, in_range, ascii.clone()),
        );
        let out = self.temp(Ty::I64);
        self.assign(Place::local(out), Rvalue::Use(cint(-1, Ty::I64)));
        let (load, slow, call, join) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.branch(ok, load, slow);
        self.switch_to(load);
        let base = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(data, Ty::Ptr));
        let Operand::Copy(bp) = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, base, at))
        else {
            ice("rvalue_temp returns a place")
        };
        let byte = Operand::Copy(proj(&bp, Proj::Deref(Ty::U8)));
        self.assign(Place::local(out), Rvalue::Cast(byte, Ty::I64));
        self.goto(join);
        // An ASCII string out of range is -1; another string asks the runtime.
        self.switch_to(slow);
        self.branch(ascii, join, call);
        self.switch_to(call);
        let s = self.addr(p);
        self.call_rt(Rt::StrCharCodeAt, vec![s, i], Some(Place::local(out)));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(out))
    }
}
