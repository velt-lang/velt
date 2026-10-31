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

/// `StringBuilder.append`, whose literal arguments are appended inline.
const STRING_BUILDER_APPEND: &str = "std/text/string_builder::StringBuilder.append";

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

    /// Append `n` ASCII bytes, which `write` stores at the pointer it is given, to the builder
    /// at `buf`: the builder's fast path (`VeltStr::push_ascii`) inline, `slow_path` (the runtime
    /// call) when it fails. The fast path: a plain heap string (not inline, not a slice: bits 62
    /// and 63 of `w2` clear, `w2` its capacity) with room for the bytes; they go after the text
    /// and `w1` gains `n` bytes and `n` units (#1677).
    ///
    /// It does not check that the buffer has no other reference, as `push_ascii` does. For
    /// `JSON.stringify`'s builder there is none: it is fresh (`velt_rt_strbuf_new`), and no code
    /// but its glue (and the Velt writers it is handed to, which only append) sees it before
    /// `finish` makes it a string. A `StringBuilder`'s buffer may be shared (`toString()`), and
    /// writing after its text is still safe: every other holder of the buffer is a string whose
    /// length ends at or before the builder's, and appends in place only once it is the only
    /// holder (the runtime checks the count), when the builder no longer has the buffer. So the
    /// bytes after the builder's text are the builder's.
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

    /// `b.append("text")` on a `StringBuilder` (string-building.md step 2, #1677): literal ASCII
    /// text appended with the builder's fast path inline, as `JSON.stringify`'s glue appends its
    /// literal text ([`Self::push_ascii_inline`] says why a shared buffer needs no count check). `None`: not such a call
    /// (it is compiled as written).
    pub(super) fn builder_append_literal(
        &mut self,
        name: &str,
        args: &[velt_sema::hir::Expr],
    ) -> Option<Operand> {
        if name != STRING_BUILDER_APPEND {
            return None;
        }
        let [recv, s] = args else {
            return None;
        };
        let velt_sema::hir::ExprKind::Lit(velt_sema::hir::Lit::Str(text)) = &s.kind else {
            return None;
        };
        if text.is_empty() || !text.is_ascii() || text.len() > INLINE_PUSH_MAX {
            return None;
        }
        let recv_ty = self.sub(recv.ty);
        let (argv, _) = self.borrow_args(std::slice::from_ref(recv), None, false, false);
        let obj = self.copy_to_temp(argv.into_iter().next()?, Ty::Ptr);
        let field = self.field_place(&Place::local(obj), recv_ty, 0);
        let buf = self.addr(field);
        let bytes = text.as_bytes();
        let n = bytes.len() as u64;
        let w1 = cint((n * 0x1_0000_0001) as i128, Ty::U64);
        if let [b] = bytes {
            let byte = cint(*b as i128, Ty::U8);
            let b2 = byte.clone();
            let slow_buf = buf.clone();
            self.push_ascii_inline(
                &buf,
                1,
                |lw, dst| {
                    let at = Place {
                        local: lw.copy_to_temp(dst, Ty::Ptr),
                        proj: vec![Proj::Deref(Ty::U8)],
                    };
                    lw.assign(at, Rvalue::Use(byte));
                },
                |lw| lw.call_rt(Rt::StrbufPushByte, vec![slow_buf, b2], None),
            );
        } else {
            let sid = self.cx.static_bytes(bytes.to_vec(), 1);
            let p = Operand::Const(Const::Static(sid), Ty::Ptr);
            let args = vec![buf.clone(), p.clone(), w1];
            self.push_ascii_inline(
                &buf,
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
        }
        Some(super::unit())
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
