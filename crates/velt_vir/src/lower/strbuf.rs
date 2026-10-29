//! Emitting calls to the runtime string builder (`velt_rt_strbuf_*`, rt_abi_async.md §12.1),
//! shared by `JSON.stringify`, the format glue behind `console.log`/`${x}` and template literals.
//! A builder has the `VeltStr` layout, so a filled builder local *is* the owned result string.

use velt_sema::hir::{TyId, TyKind};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Stmt, Ty, STR_AGG};

/// The longest literal text appended with the inline fast path (longer text is one call).
const INLINE_PUSH_MAX: usize = 32;

impl FnLower<'_, '_> {
    /// A fresh empty builder local with room for `cap` bytes (0 = allocate on first push),
    /// and its address.
    pub(super) fn new_strbuf(&mut self, cap: u64) -> (vir::Local, Operand) {
        let buf = self.temp(Ty::Agg(STR_AGG));
        let bp = self.addr(Place::local(buf));
        let args = vec![cint(cap as i128, Ty::U64), bp.clone()];
        self.call_rt(Rt::StrbufNew, args, None);
        (buf, bp)
    }

    /// [`new_strbuf`](Self::new_strbuf) with a capacity computed at run time (a `u64`).
    pub(super) fn new_strbuf_sized(&mut self, cap: Operand) -> (vir::Local, Operand) {
        let buf = self.temp(Ty::Agg(STR_AGG));
        let bp = self.addr(Place::local(buf));
        self.call_rt(Rt::StrbufNew, vec![cap, bp.clone()], None);
        (buf, bp)
    }

    /// Append static text to the builder at `buf` (one byte, or a static chunk).
    pub(super) fn push_text(&mut self, buf: &Operand, text: &str) {
        match text.as_bytes() {
            [] => {}
            [b] => self.call_rt(
                Rt::StrbufPushByte,
                vec![buf.clone(), cint(*b as i128, Ty::U8)],
                None,
            ),
            bytes => {
                let sid = self.cx.static_bytes(bytes.to_vec(), 1);
                let p = Operand::Const(Const::Static(sid), Ty::Ptr);
                // The length argument carries the unit count too (rt_abi_async.md §12.1).
                let w1 = super::strings::str_w1(text);
                let args = vec![buf.clone(), p, cint(w1 as i128, Ty::U64)];
                self.call_rt(Rt::StrbufPushBytes, args, None);
            }
        }
    }

    /// [`Self::push_text`] with the builder's fast path inline (#1677), for `JSON.stringify`'s
    /// glue, whose builder no other reference can reach. Each site costs about ten instructions
    /// more than a call (bench/json/stringify: 14-17% faster; IR of programs that stringify
    /// 5-13% larger), so other builders (template literals, `console.log`, error messages) keep
    /// the call.
    pub(super) fn push_text_private(&mut self, buf: &Operand, text: &str) {
        match text.as_bytes() {
            [] => {}
            [b] if b.is_ascii() => {
                let byte = cint(*b as i128, Ty::U8);
                let b2 = byte.clone();
                self.push_ascii_inline(
                    buf,
                    1,
                    |lw, dst| {
                        let at = Place {
                            local: lw.copy_to_temp(dst, Ty::Ptr),
                            proj: vec![Proj::Deref(Ty::U8)],
                        };
                        lw.assign(at, Rvalue::Use(byte));
                    },
                    |lw| lw.call_rt(Rt::StrbufPushByte, vec![buf.clone(), b2], None),
                );
            }
            [b] => self.call_rt(
                Rt::StrbufPushByte,
                vec![buf.clone(), cint(*b as i128, Ty::U8)],
                None,
            ),
            bytes => {
                let sid = self.cx.static_bytes(bytes.to_vec(), 1);
                let p = Operand::Const(Const::Static(sid), Ty::Ptr);
                if text.is_ascii() && bytes.len() <= INLINE_PUSH_MAX {
                    let n = bytes.len() as u64;
                    // ASCII: as many units as bytes (the high half of the length).
                    let w1 = cint((n * 0x1_0000_0001) as i128, Ty::U64);
                    let args = vec![buf.clone(), p.clone(), w1];
                    self.push_ascii_inline(
                        buf,
                        n,
                        |lw, dst| {
                            lw.push_stmt(Stmt::MemCopy {
                                dst,
                                src: p,
                                size: n,
                            })
                        },
                        |lw| lw.call_rt(Rt::StrbufPushBytes, args, None),
                    );
                    return;
                }
                // The length argument carries the unit count too (rt_abi_async.md §12.1).
                let w1 = super::strings::str_w1(text);
                let args = vec![buf.clone(), p, cint(w1 as i128, Ty::U64)];
                self.call_rt(Rt::StrbufPushBytes, args, None);
            }
        }
    }

    /// Append `n` ASCII bytes, which `write` stores at the pointer it is given, to the builder
    /// at `buf`: the builder's fast path (`VeltStr::push_ascii`) inline, `slow_path` (the runtime
    /// call) when it fails. The fast path: a plain heap string (not inline, not a slice: bits 62
    /// and 63 of `w2` clear, `w2` its capacity) with room for the bytes; they go after the text
    /// and `w1` gains `n` bytes and `n` units (#1677).
    ///
    /// It does not check that the buffer has no other reference, as `push_ascii` does: `buf` is
    /// `JSON.stringify`'s builder, which has none. It is fresh (`velt_rt_strbuf_new`), and no
    /// code but its glue (and the Velt writers it is handed to, which only append) sees it
    /// before `finish` makes it a string.
    fn push_ascii_inline(
        &mut self,
        buf: &Operand,
        n: u64,
        write: impl FnOnce(&mut Self, Operand),
        slow_path: impl FnOnce(&mut Self),
    ) {
        let p = Place {
            local: self.copy_to_temp(buf.clone(), Ty::Ptr),
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
        };
        let w0 = Operand::Copy(proj(&p, Proj::Field(0)));
        let w1 = self.copy_to_temp(Operand::Copy(proj(&p, Proj::Field(1))), Ty::U64);
        let w2 = self.copy_to_temp(Operand::Copy(proj(&p, Proj::Field(2))), Ty::U64);
        let (w1, w2) = (
            Operand::Copy(Place::local(w1)),
            Operand::Copy(Place::local(w2)),
        );
        let len = self.u64_op(BinOp::BitAnd, w1.clone(), cint(0xffff_ffff, Ty::U64));
        let end = self.u64_op(BinOp::Add, len.clone(), cint(n as i128, Ty::U64));
        // Heap, not a slice (w2 below 2^62 and not 0), and room: `0 < w2`, `w2 < 2^62` and
        // `end <= w2` in two compares, since `end` is at least 1.
        let tag = self.u64_op(BinOp::UShr, w2.clone(), cint(62, Ty::U64));
        let plain = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, cint(0, Ty::U64)));
        let room = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, end.clone(), w2));
        let ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, plain, room));
        let (fast, slow, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(ok, fast, slow);
        self.switch_to(fast);
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(w0, Ty::Ptr));
        let dst = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, len));
        write(self, dst);
        let grown = self.u64_op(BinOp::Add, w1, cint((n * 0x1_0000_0001) as i128, Ty::U64));
        self.assign(proj(&p, Proj::Field(1)), Rvalue::Use(grown));
        self.goto(done);
        self.switch_to(slow);
        slow_path(self);
        self.goto(done);
        self.switch_to(done);
    }

    /// Append the string at address `s` to the builder at `buf`.
    pub(super) fn push_str(&mut self, buf: &Operand, s: Operand) {
        self.call_rt(Rt::StrbufPushStr, vec![buf.clone(), s], None);
    }

    /// Append the float `v` of type `ty` the way `console.log` prints it (`-0` is `-0`).
    pub(super) fn push_inspect_float(&mut self, buf: &Operand, v: Operand, ty: TyId) {
        let from = self.cx.ty(ty);
        let v = self.cast_to(v, from, Ty::F64);
        self.call_rt(Rt::StrbufPushInspectF64, vec![buf.clone(), v], None);
    }

    /// Append an int/float/bool (or C-like enum discriminant) `v` of type `ty` in JS format.
    pub(super) fn push_scalar(&mut self, buf: &Operand, v: Operand, ty: TyId) {
        let from = self.cx.ty(ty);
        let b = buf.clone();
        match self.cx.kind(ty) {
            TyKind::Float(_) => {
                let v = self.cast_to(v, from, Ty::F64);
                self.call_rt(Rt::StrbufPushF64, vec![b, v], None);
            }
            TyKind::Bool => self.call_rt(Rt::StrbufPushBool, vec![b, v], None),
            TyKind::Int(it) if !it.is_signed() => {
                let v = self.cast_to(v, from, Ty::U64);
                self.call_rt(Rt::StrbufPushU64, vec![b, v], None);
            }
            _ => {
                let v = self.cast_to(v, from, Ty::I64);
                self.call_rt(Rt::StrbufPushI64, vec![b, v], None);
            }
        }
    }
}
