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

/// std's number formatting in Velt (std/lang/number/format.vlt): integers appended to a builder,
/// and as a string of their own.
const INT_PUSH: &str = velt_common::std_hook!(int_push);
const UINT_PUSH: &str = velt_common::std_hook!(uint_push);
const NUMBER_PUSH: &str = velt_common::std_hook!(number_push);
pub(super) const NUMBER_TEXT: &str = velt_common::std_hook!(number_text);
pub(super) const INT_TEXT: &str = velt_common::std_hook!(int_text);
pub(super) const UINT_TEXT: &str = velt_common::std_hook!(uint_text);

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

    /// Call std's number hook `name` (no type parameters) with `args`, its result (if any)
    /// returned through `out`. False when the program has no such hook (a program built
    /// without std, as `velt_vir`'s own tests build them) or it is unchecked: the caller calls
    /// the runtime.
    pub(super) fn call_number_hook(
        &mut self,
        name: &str,
        mut args: Vec<Operand>,
        out: Option<Place>,
    ) -> bool {
        // An on-demand hook that sema left unchecked (the program converts no value that may
        // hold a float to text, as far as sema saw: velt_common `std_hooks::ON_DEMAND`) is
        // missing too.
        let Some(d) = self
            .cx
            .prelude_def(name)
            .filter(|d| !self.cx.hir.unchecked.contains(d))
        else {
            return false;
        };
        let f = self.cx.func_for_direct(d, vec![], self.mode);
        if let Some(o) = out {
            let a = self.addr(o);
            args.push(a);
        }
        self.call(vir::Callee::Func(f), args, None, false);
        true
    }

    /// Expression `e` (an integer) as a `u64`.
    pub(super) fn expr_as_u64(&mut self, e: &velt_sema::hir::Expr) -> Operand {
        let v = self.expr(e);
        let t = self.sub(e.ty);
        let from = self.cx.ty(t);
        self.cast_to(v, from, Ty::U64)
    }

    /// `strAsciiWords(w0, w1, w2, len)`: the inline ASCII string of `len` (at most 23) bytes
    /// held in the words (bytes past `len` are 0, std's promise): the words, with byte 23 the
    /// inline tag `0x80 | len` (rt_abi.md "Strings"). No call.
    pub(super) fn str_ascii_words(&mut self, w: [Operand; 3], len: Operand, ty: TyId) -> Operand {
        let [w0, w1, w2] = w;
        let out = self.temp(Ty::Agg(STR_AGG));
        let low = self.u64_op(BinOp::BitAnd, w2, cint(0x00ff_ffff_ffff_ffff, Ty::U64));
        let tag = self.u64_op(BinOp::BitOr, len, cint(0x80, Ty::U64));
        let tag = self.u64_op(BinOp::Shl, tag, cint(56, Ty::U64));
        let w2 = self.u64_op(BinOp::BitOr, low, tag);
        for (i, v) in [w0, w1, w2].into_iter().enumerate() {
            self.assign(
                proj(&Place::local(out), Proj::Field(i as u32)),
                Rvalue::Use(v),
            );
        }
        self.own_temp(out, ty);
        Operand::Copy(Place::local(out))
    }

    /// `strbufPushAsciiWords(buf, w0, w1, w2, len)`: append the first `len` (at most 24) bytes
    /// of the words, ASCII, to the builder at `buf`. Fast path as [`Self::push_ascii_inline`]'s
    /// (which says why the bytes after a builder's text are its own), with room for all 24
    /// bytes: the words are copied there whole and the length grows by `len`. Otherwise an
    /// inline builder that still fits gets the words OR'd in, and any other calls
    /// `velt_rt_strbuf_push_words` with them by value.
    pub(super) fn strbuf_push_ascii_words(&mut self, buf: Operand, w: [Operand; 3], n: Operand) {
        let words = self.temp(Ty::Agg(STR_AGG));
        for (i, v) in w.into_iter().enumerate() {
            self.assign(
                proj(&Place::local(words), Proj::Field(i as u32)),
                Rvalue::Use(v),
            );
        }
        let n = self.copy_to_temp(n, Ty::U64);
        let n = Operand::Copy(Place::local(n));
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
        let end = self.u64_op(BinOp::Add, len.clone(), cint(24, Ty::U64));
        let tag = self.u64_op(BinOp::UShr, w2.clone(), cint(62, Ty::U64));
        let plain = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, cint(0, Ty::U64)));
        let room = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, end, w2.clone()));
        let ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, plain, room));
        let (fast, slow, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(ok, fast, slow);
        self.switch_to(fast);
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(w0, Ty::Ptr));
        let dst = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, len));
        let src = self.addr(Place::local(words));
        self.push_stmt(Stmt::MemCopy { dst, src, size: 24 });
        let both = self.u64_op(BinOp::Mul, n.clone(), cint(0x1_0000_0001, Ty::U64));
        let grown = self.u64_op(BinOp::Add, w1, both);
        self.assign(proj(&p, Proj::Field(1)), Rvalue::Use(grown));
        self.goto(done);
        self.switch_to(slow);
        // An inline ASCII builder (byte 23 is `0x80 | len`) that still fits in 23 bytes: the words
        // go in after its text, in place. Short templates start inline (`user ${i}`).
        let (inline, call) = (self.new_block(), self.new_block());
        let tag = self.u64_op(BinOp::UShr, w2.clone(), cint(56, Ty::U64));
        let kind = self.u64_op(BinOp::BitAnd, tag.clone(), cint(0xe0, Ty::U64));
        let ascii = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, kind, cint(0x80, Ty::U64)),
        );
        let cur = self.u64_op(BinOp::BitAnd, tag, cint(0x1f, Ty::U64));
        let total = self.u64_op(BinOp::Add, cur.clone(), n.clone());
        let fits = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Le, total.clone(), cint(23, Ty::U64)),
        );
        let both = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, ascii, fits));
        self.branch(both, inline, call);
        self.switch_to(inline);
        self.inline_append_words(&p, words, cur, total);
        self.goto(done);
        self.switch_to(call);
        // The words by value: no stack for them, so the call can be the caller's tail call.
        let word = |i: u32| Operand::Copy(proj(&Place::local(words), Proj::Field(i)));
        let args = vec![buf, word(0), word(1), word(2), n];
        self.call_rt(Rt::StrbufPushWords, args, None);
        self.goto(done);
        self.switch_to(done);
    }

    /// `strbufPushAsciiWord(buf, w, len)`: append the first `len` (at most 8) bytes of `w`,
    /// ASCII, to the builder at `buf`. An inline ASCII builder of fewer than 16 bytes (byte 23 is
    /// `0x80 | cur`, `cur < 16`: one test of `byte23 & 0xf0`) gets the word stored at byte `cur`
    /// of the builder itself and byte 23 raised by `len`: the store ends by byte 22, and the
    /// word's bytes past `len` are 0, as the builder's bytes past its text are. A plain heap
    /// builder with room for 8 more bytes gets the word stored after its text and its length
    /// grown. Anything else calls `velt_rt_strbuf_push_words`. Short templates start inline
    /// (`` `w${i}` ``, `` `user ${i}` ``), so their path is the first and needs no shifts.
    pub(super) fn strbuf_push_ascii_word(&mut self, buf: Operand, w: Operand, n: Operand) {
        let w = self.copy_to_temp(w, Ty::U64);
        let w = Operand::Copy(Place::local(w));
        let wsrc = |lw: &mut Self| {
            let l = match &w {
                Operand::Copy(pl) => pl.local,
                _ => super::ice("a copied word"),
            };
            lw.addr(Place::local(l))
        };
        let n = self.copy_to_temp(n, Ty::U64);
        let n = Operand::Copy(Place::local(n));
        let base = self.copy_to_temp(buf.clone(), Ty::Ptr);
        let p = Place {
            local: base,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
        };
        let field = |i: u32| Operand::Copy(proj(&p, Proj::Field(i)));
        let w2 = self.copy_to_temp(field(2), Ty::U64);
        let w2 = Operand::Copy(Place::local(w2));
        let (inline, other, heap, call, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let tag = self.u64_op(BinOp::UShr, w2.clone(), cint(56, Ty::U64));
        let kind = self.u64_op(BinOp::BitAnd, tag.clone(), cint(0xf0, Ty::U64));
        let short = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, kind, cint(0x80, Ty::U64)),
        );
        self.branch(short, inline, other);
        self.switch_to(inline);
        let cur = self.u64_op(BinOp::BitAnd, tag.clone(), cint(0x1f, Ty::U64));
        let at = Operand::Copy(Place::local(base));
        let dst = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, at.clone(), cur));
        let src = wsrc(self);
        self.push_stmt(Stmt::MemCopy { dst, src, size: 8 });
        // `0x80 | (cur + len)`: `cur + len` is at most 23, so the sum doesn't reach the kind bits.
        let t = self.u64_op(BinOp::Add, tag, n.clone());
        let t = self.rvalue_temp(Ty::U8, Rvalue::Cast(t, Ty::U8));
        let top = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, at, cint(23, Ty::U64)),
        );
        self.mem_set(top, t, cint(1, Ty::U64));
        self.goto(done);
        self.switch_to(other);
        let w1 = self.copy_to_temp(field(1), Ty::U64);
        let w1 = Operand::Copy(Place::local(w1));
        let len = self.u64_op(BinOp::BitAnd, w1.clone(), cint(0xffff_ffff, Ty::U64));
        let end = self.u64_op(BinOp::Add, len.clone(), cint(8, Ty::U64));
        let kind = self.u64_op(BinOp::UShr, w2.clone(), cint(62, Ty::U64));
        let plain = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, kind, cint(0, Ty::U64)));
        let room = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, end, w2));
        let ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, plain, room));
        self.branch(ok, heap, call);
        self.switch_to(heap);
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(field(0), Ty::Ptr));
        let dst = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, len));
        let src = wsrc(self);
        self.push_stmt(Stmt::MemCopy { dst, src, size: 8 });
        let both = self.u64_op(BinOp::Mul, n.clone(), cint(0x1_0000_0001, Ty::U64));
        let grown = self.u64_op(BinOp::Add, w1, both);
        self.assign(proj(&p, Proj::Field(1)), Rvalue::Use(grown));
        self.goto(done);
        self.switch_to(call);
        let zero = || cint(0, Ty::U64);
        self.call_rt(Rt::StrbufPushWords, vec![buf, w, zero(), zero(), n], None);
        self.goto(done);
        self.switch_to(done);
    }

    /// The inline string at `p` (ASCII, `cur` bytes) with the words of `words` after its text,
    /// `total` bytes in all (at most 23): the words shifted `cur % 8` bytes on, OR'd into the
    /// string's words from word `cur / 8` (its bytes past its text are 0), and byte 23 rewritten
    /// as `0x80 | total`. The word offset is a branch: a loop appends at the same place each
    /// time, so it is predicted, and costs less than selecting words by masks.
    fn inline_append_words(&mut self, p: &Place, words: vir::Local, cur: Operand, total: Operand) {
        let word = |i: u32| Operand::Copy(proj(&Place::local(words), Proj::Field(i)));
        let (a0, a1, a2) = (word(0), word(1), word(2));
        let r = self.u64_op(BinOp::BitAnd, cur.clone(), cint(7, Ty::U64));
        let r = self.u64_op(BinOp::Mul, r, cint(8, Ty::U64));
        let q = self.u64_op(BinOp::UShr, cur, cint(3, Ty::U64));
        // `(a >> 1) >> (63 - r)` is `a >> (64 - r)`, and 0 when `r` is 0.
        let back = self.u64_op(BinOp::Sub, cint(63, Ty::U64), r.clone());
        let carry = |lw: &mut Self, a: Operand| {
            let half = lw.u64_op(BinOp::UShr, a, cint(1, Ty::U64));
            lw.u64_op(BinOp::UShr, half, back.clone())
        };
        let s0 = self.u64_op(BinOp::Shl, a0.clone(), r.clone());
        let c0 = carry(self, a0);
        let s1 = self.u64_op(BinOp::Shl, a1.clone(), r.clone());
        let s1 = self.u64_op(BinOp::BitOr, s1, c0);
        let c1 = carry(self, a1);
        let s2 = self.u64_op(BinOp::Shl, a2, r);
        let s2 = self.u64_op(BinOp::BitOr, s2, c1);
        // OR the shifted words in from word `q` (0, 1 or 2).
        let or_into = |lw: &mut Self, i: u32, v: &Operand| {
            let old = Operand::Copy(proj(p, Proj::Field(i)));
            let new = lw.u64_op(BinOp::BitOr, old, v.clone());
            lw.assign(proj(p, Proj::Field(i)), Rvalue::Use(new));
        };
        let (at0, rest, at1, at2, tag) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let first = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, q.clone(), cint(0, Ty::U64)),
        );
        self.branch(first, at0, rest);
        self.switch_to(at0);
        or_into(self, 0, &s0);
        or_into(self, 1, &s1);
        or_into(self, 2, &s2);
        self.goto(tag);
        self.switch_to(rest);
        let second = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, q, cint(1, Ty::U64)));
        self.branch(second, at1, at2);
        self.switch_to(at1);
        or_into(self, 1, &s0);
        or_into(self, 2, &s1);
        self.goto(tag);
        self.switch_to(at2);
        or_into(self, 2, &s0);
        self.goto(tag);
        self.switch_to(tag);
        let old = Operand::Copy(proj(p, Proj::Field(2)));
        let w2 = self.u64_op(BinOp::BitAnd, old, cint(0x00ff_ffff_ffff_ffff, Ty::U64));
        let tag = self.u64_op(BinOp::BitOr, total, cint(0x80, Ty::U64));
        let tag = self.u64_op(BinOp::Shl, tag, cint(56, Ty::U64));
        let w2 = self.u64_op(BinOp::BitOr, w2, tag);
        self.assign(proj(p, Proj::Field(2)), Rvalue::Use(w2));
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
        let v = self.float_as_f64(v, ty);
        self.call_rt(Rt::StrbufPushInspectF64, vec![buf.clone(), v], None);
    }

    /// Append an int/float/bool (or C-like enum discriminant) `v` of type `ty` in JS format.
    pub(super) fn push_scalar(&mut self, buf: &Operand, v: Operand, ty: TyId) {
        let from = self.cx.ty(ty);
        let b = buf.clone();
        match self.cx.kind(ty) {
            TyKind::Float(_) => {
                let v = self.float_as_f64(v, ty);
                if !self.call_number_hook(NUMBER_PUSH, vec![b.clone(), v.clone()], None) {
                    self.call_rt(Rt::StrbufPushF64, vec![b, v], None);
                }
            }
            TyKind::Bool => self.call_rt(Rt::StrbufPushBool, vec![b, v], None),
            TyKind::Int(it) if !it.is_signed() => {
                let v = self.cast_to(v, from, Ty::U64);
                if !self.call_number_hook(UINT_PUSH, vec![b.clone(), v.clone()], None) {
                    self.call_rt(Rt::StrbufPushU64, vec![b, v], None);
                }
            }
            TyKind::Int(_) => {
                let v = self.cast_to(v, from, Ty::I64);
                if !self.call_number_hook(INT_PUSH, vec![b.clone(), v.clone()], None) {
                    self.call_rt(Rt::StrbufPushI64, vec![b, v], None);
                }
            }
            _ => {
                let v = self.cast_to(v, from, Ty::I64);
                self.call_rt(Rt::StrbufPushI64, vec![b, v], None);
            }
        }
    }
}
