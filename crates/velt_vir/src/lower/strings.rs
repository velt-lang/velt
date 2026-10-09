//! Reading strings inline (rt_abi.md "Strings"): `s.length` and `s.charCodeAt(i)` of an ASCII
//! string need no runtime call. A string is `{w0, w1, w2}`; the inline form has the top bit of
//! `w2` set, its byte length in bits 56..61 of `w2` (byte 23), bit 62 set when it is not ASCII,
//! whose unit count is then byte 22, and its bytes in the value itself; the static and heap forms
//! keep `{ptr, units << 32 | len}` in `w0`/`w1`. `length` counts UTF-16 code units (#377 phase
//! 2b); a string is ASCII exactly when its unit count equals its byte count, and only then does
//! `charCodeAt` read a byte.
//!
//! The shapes were chosen by instruction counts on bench/strings, whose scan loop
//! (`for (i < s.length) s.charCodeAt(i)`) runs both reads per character, on both backends. Both
//! load the words first and then branch on the form into arms of plain arithmetic: Cranelift
//! code runs only the taken arm (select chains cost it 60% there), and LLVM unswitches the loop
//! per form. `charCodeAt` bounds-checks an ASCII string against its unit count, the loop's own
//! bound, with signed compares like the loop's test, so LLVM proves the check away (an unsigned
//! check against the byte length cost it 13%). No value is read as a zero-extended 32-bit
//! number, which lets LLVM vectorize scan loops into slower SSE2 code (#377 phase 1 measured
//! 16%); the counts are read as signed 32-bit numbers instead (exact below 2 GiB).

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
    pub(super) fn u64_op(&mut self, op: BinOp, a: Operand, b: Operand) -> Operand {
        self.rvalue_temp(Ty::U64, Rvalue::Binary(op, a, b))
    }

    /// `mask ? a : b` for a mask of all ones / all zeros.
    pub(super) fn select(&mut self, mask: Operand, a: Operand, b: Operand) -> Operand {
        let not = self.rvalue_temp(Ty::U64, Rvalue::Unary(UnOp::BitNot, mask.clone()));
        let a = self.u64_op(BinOp::BitAnd, a, mask);
        let b = self.u64_op(BinOp::BitAnd, b, not);
        self.u64_op(BinOp::BitOr, a, b)
    }

    /// Words 1 and 2 of the string at `p`, and whether it is inline (the sign of `w2`).
    fn str_words(&mut self, p: &Place) -> (Operand, Operand, Operand) {
        let word = |i| Operand::Copy(proj(p, Proj::Field(i)));
        let w1 = self.rvalue_temp(Ty::U64, Rvalue::Use(word(1)));
        let w2 = self.rvalue_temp(Ty::U64, Rvalue::Use(word(2)));
        let signed = self.rvalue_temp(Ty::I64, Rvalue::Cast(w2.clone(), Ty::I64));
        let inline = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, signed, cint(0, Ty::I64)),
        );
        (w1, w2, inline)
    }

    /// The halves of `w1` as signed 32-bit numbers: `(units, bytes)` of a static or heap string
    /// (the high half as [`Self::str_high`] computes it).
    fn str_halves(&mut self, w1: Operand) -> (Operand, Operand) {
        let high = self.str_high(w1.clone());
        let high = self.rvalue_temp(Ty::I64, Rvalue::Cast(high, Ty::I64));
        let low = self.rvalue_temp(Ty::I32, Rvalue::Cast(w1, Ty::I32));
        let low = self.rvalue_temp(Ty::I64, Rvalue::Cast(low, Ty::I64));
        (high, low)
    }

    /// The high half of `w1` (a static or heap string's UTF-16 length), below 2^31: a string is
    /// shorter than 2 GiB (rt_abi.md), and the mask says so to the optimizers (`numrep` bounds
    /// loops to `s.length` with it; LLVM sees that `str_units` and `str_halves` give the same
    /// value, so a loop to `s.length` reading `s.charCodeAt(i)` needs no range test).
    fn str_high(&mut self, w1: Operand) -> Operand {
        let high = self.u64_op(BinOp::UShr, w1, cint(32, Ty::U64));
        self.u64_op(BinOp::BitAnd, high, cint(0x7fff_ffff, Ty::U64))
    }

    /// Bits of byte 23 (`w2 >> 56`) masked with `mask`.
    fn tag_bits(&mut self, w2: Operand, mask: i128) -> Operand {
        let top = self.u64_op(BinOp::UShr, w2, cint(56, Ty::U64));
        self.u64_op(BinOp::BitAnd, top, cint(mask, Ty::U64))
    }

    /// The byte length (a `u64`) of the string at `p`: the low half of `w1` for a static or heap
    /// string, byte 23's length bits for an inline one. No branch.
    pub(super) fn str_bytes(&mut self, p: &Place) -> Operand {
        let w1 = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(1)))));
        let w2 = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(2)))));
        let inline_len = self.tag_bits(w2.clone(), 0x1f);
        let heap_len = self.u64_op(BinOp::BitAnd, w1, cint(0xffff_ffff, Ty::U64));
        let signed = self.rvalue_temp(Ty::I64, Rvalue::Cast(w2, Ty::I64));
        let fill = self.rvalue_temp(
            Ty::I64,
            Rvalue::Binary(BinOp::Shr, signed, cint(63, Ty::I64)),
        );
        let inline = self.rvalue_temp(Ty::U64, Rvalue::Cast(fill, Ty::U64));
        self.select(inline, inline_len, heap_len)
    }

    /// `s.length` (a `u64`): UTF-16 code units. Inline: byte 22 when bit 0x40 of byte 23 says
    /// the string is not ASCII, else byte 23's length; otherwise the high half of `w1`.
    pub(super) fn str_len(&mut self, v: Operand) -> Operand {
        let p = self.operand_place(v, Ty::Agg(STR_AGG));
        self.str_units(&p)
    }

    /// The UTF-16 length of the string at `p` (see [`Self::str_len`]).
    fn str_units(&mut self, p: &Place) -> Operand {
        let (w1, w2, inline) = self.str_words(p);
        let out = Place::local(self.temp(Ty::U64));
        let (inl, outl, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(inline, inl, outl);
        self.switch_to(inl);
        let short = self.tag_bits(w2.clone(), 0x1f);
        let b22 = self.u64_op(BinOp::UShr, w2.clone(), cint(48, Ty::U64));
        let b22 = self.u64_op(BinOp::BitAnd, b22, cint(0xff, Ty::U64));
        // All ones when bit 0x40 of byte 23 (bit 62) is set.
        let shifted = self.u64_op(BinOp::Shl, w2, cint(1, Ty::U64));
        let signed = self.rvalue_temp(Ty::I64, Rvalue::Cast(shifted, Ty::I64));
        let fill = self.rvalue_temp(
            Ty::I64,
            Rvalue::Binary(BinOp::Shr, signed, cint(63, Ty::I64)),
        );
        let non_ascii = self.rvalue_temp(Ty::U64, Rvalue::Cast(fill, Ty::U64));
        let units = self.select(non_ascii, b22, short);
        self.assign(out.clone(), Rvalue::Use(units));
        self.goto(join);
        self.switch_to(outl);
        let high = self.str_high(w1);
        self.assign(out.clone(), Rvalue::Use(high));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// `s.charCodeAt(i)`: the UTF-16 code unit at `i` as an `i64`, or -1 when `i` is out of range.
    /// An ASCII string (units == bytes) loads the byte inline, as before #377 phase 2b; any other
    /// string calls the runtime, which translates the position.
    pub(super) fn str_char_code_at(&mut self, v: Operand, i: Operand) -> Operand {
        let p = self.operand_place(v, Ty::Agg(STR_AGG));
        let (w1, w2, inline) = self.str_words(&p);
        let ptr = Operand::Copy(proj(&p, Proj::Field(0)));
        let here = self.addr(p.clone());
        let len = Place::local(self.temp(Ty::U64));
        let data = Place::local(self.temp(Ty::Ptr));
        let out = self.temp(Ty::I64);
        let (inl, outl, ascii_blk, nonneg_blk, load, call, join) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        // Per form: not ASCII goes to the runtime; otherwise the length (units == bytes) and
        // where the bytes are. Each form branches on its own test: no flag to materialize.
        self.branch(inline, inl, outl);
        self.switch_to(inl);
        let short = self.tag_bits(w2.clone(), 0x1f);
        self.assign(len.clone(), Rvalue::Use(short));
        self.assign(data.clone(), Rvalue::Use(here));
        let flag = self.tag_bits(w2, 0x40);
        let ascii = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, flag, cint(0, Ty::U64)));
        self.branch(ascii, ascii_blk, call);
        self.switch_to(outl);
        let (units, bytes) = self.str_halves(w1);
        // The unit count, what `length` reads: a `for (i < s.length)` loop's own bound.
        self.assign(len.clone(), Rvalue::Cast(units.clone(), Ty::U64));
        self.assign(data.clone(), Rvalue::Cast(ptr, Ty::Ptr));
        let ascii = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, units, bytes));
        self.branch(ascii, ascii_blk, call);
        // `0 <= i < len` as signed compares, the form of the loop's own test
        // (`i < s.length as i64`), which LLVM's induction analysis then proves in the loop's
        // ASCII version. Branches rather than a combined flag: fewer instructions in Cranelift.
        self.switch_to(ascii_blk);
        self.assign(Place::local(out), Rvalue::Use(cint(-1, Ty::I64)));
        let nonneg = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ge, i.clone(), cint(0, Ty::I64)),
        );
        self.branch(nonneg, nonneg_blk, join);
        self.switch_to(nonneg_blk);
        let len = self.rvalue_temp(Ty::I64, Rvalue::Cast(Operand::Copy(len), Ty::I64));
        let below = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, i.clone(), len));
        self.branch(below, load, join);
        self.switch_to(load);
        let at = self.rvalue_temp(Ty::U64, Rvalue::Cast(i.clone(), Ty::U64));
        let Operand::Copy(bp) = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, Operand::Copy(data), at),
        ) else {
            ice("rvalue_temp returns a place")
        };
        let byte = Operand::Copy(proj(&bp, Proj::Deref(Ty::U8)));
        self.assign(Place::local(out), Rvalue::Cast(byte, Ty::I64));
        self.goto(join);
        self.switch_to(call);
        let s = self.addr(p);
        self.call_rt(Rt::StrCharCodeAt, vec![s, i], Some(Place::local(out)));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(out))
    }
}
