//! Views of bytes passed to a system function's buffer parameter (#1718; sema's
//! `body/expr/system_args.rs`): the address and the length of the bytes, with no copy.
//!
//! - `xs.subarray(begin, end)` of a `u8[]` (`Intrinsic::Subarray`): the bytes between JS's
//!   `subarray` positions of `begin` and `end` (negative counts from the end, clamped).
//! - A typed array or a `DataView` (a class extending std's `BufferView`, which holds its
//!   `ArrayBuffer` `buf`, the byte offset `off` and the length `len` in elements): the bytes
//!   `off..off + byteLength` of the buffer's `u8[]`, whose header is read when the C arguments
//!   are assembled, after every argument (a later one may grow the array, which moves its
//!   bytes, or shrink it). The view's owner may have changed length since it was made: bytes
//!   outside its current length stop the program.

use velt_sema::hir::{self, Callee as HCallee, Intrinsic, TyId};

use super::operand::proj;
use super::system::Arg;
use super::{cint, ice, FnLower};
use crate::system::Func;
use crate::vir::{BinOp, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The std function giving a view's length in bytes (std/typed_array/core.vlt).
const VIEW_BYTES: &str = "std/typed_array/core::__viewBytes";

impl FnLower<'_, '_> {
    /// The bytes of `a`, when it is a view: `None` for a `u8[]` (the caller takes its header).
    ///
    /// `xs.subarray(begin, end)` evaluates `xs`, `begin` and `end` in that order and keeps the
    /// array and the two positions ([`Arg::Sub`]). The array is the one `xs` held then (JS
    /// order), retained when a later operand could replace it; its header is read and the
    /// positions are clamped only when the C arguments are assembled ([`Self::subarray_bytes`]),
    /// after every argument, so a later one (or a position) that grows the array cannot leave
    /// the pointer at freed bytes.
    pub(super) fn system_view(
        &mut self,
        func: &Func,
        name: &str,
        a: &hir::Expr,
        later: super::sequence::Later,
    ) -> Option<Arg> {
        if let hir::ExprKind::Call {
            callee: HCallee::Intrinsic(Intrinsic::Subarray),
            args,
        } = &a.kind
        {
            let [xs, begin, end] = args.as_slice() else {
                ice("`Subarray` takes three arguments")
            };
            // Retain the array across the positions and the later arguments: one that
            // reassigns a module `let` (or otherwise drops the owner) must not free it.
            let after = later.or(self.later_of(begin)).or(self.later_of(end));
            self.note_held_buffer(xs, after);
            let v = self.expr(xs);
            let v = self.hold_buffer(v, xs.ty, after);
            let t = self.sub(xs.ty);
            let vt = self.cx.ty(t);
            let place = self.operand_place(v, vt);
            let b = self.expr(begin);
            let b = self.freeze(b, begin.ty);
            let e = self.expr(end);
            let e = self.freeze(e, end.ty);
            return Some(Arg::Sub {
                place,
                t,
                begin: (b, begin.ty),
                end: (e, end.ty),
            });
        }
        let t = self.sub(a.ty);
        if !self.cx.is_class(t) {
            return None;
        }
        // Retain the view (which holds its `ArrayBuffer`, and so the `u8[]`) across later
        // arguments that may free it.
        self.note_held_buffer(a, later);
        let v = self.expr(a);
        let v = self.hold_buffer(v, a.ty, later);
        let place = self.operand_place(v, Ty::Ptr);
        Some(Arg::View {
            place,
            t,
            outside: format!(
                "system function `{}`: the bytes `{name}` views are outside its buffer, which \
                 is shorter than when the view was made",
                func.name
            ),
        })
    }

    /// The address and length (`u64`) of the bytes of the view of class type `t` whose pointer
    /// is at `place`: `off..off + byteLength` of its buffer's `u8[]` as it is now (its header
    /// read here, at the call, after every argument: a later one may have grown, shrunk or
    /// replaced the array), or the program stops with `outside` when they are not all in it.
    pub(super) fn view_bytes(
        &mut self,
        place: &Place,
        t: TyId,
        outside: &str,
    ) -> (Operand, Operand) {
        let v = Operand::Copy(place.clone());
        let buf = self.class_field(place, t, "buf");
        let off = self.class_field(place, t, "off");
        let off = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(off.0)));
        let (buf, bt) = buf;
        let (bytes, at) = self.class_field(&buf, bt, "bytes");
        let (data, blen) = self.header_parts(&bytes, at);
        // `len * __size()`, through std (a typed array's element size is a method). A view the
        // program forged by overriding `__size()` or with a constructor of its own can make
        // this huge or `off` wrap; the two checks below are non-wrapping, so the range stays
        // inside the buffer's own bytes whatever the view claims.
        let d = self
            .cx
            .prelude_def(VIEW_BYTES)
            .unwrap_or_else(|| ice("no `__viewBytes` in std/typed_array/core"));
        let f = self.cx.func_for_direct(d, vec![], self.mode);
        let n = Place::local(self.temp(Ty::U64));
        self.call(Callee::Func(f), vec![v], Some(n.clone()), false);
        let n = Operand::Copy(n);
        // `off <= blen && n <= blen - off`, checked without the addition that could wrap: the
        // start is inside the buffer, and the length fits what is left after it.
        let (ok, bad, span) = (self.new_block(), self.new_block(), self.new_block());
        let start_ok = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Le, off.clone(), blen.clone()),
        );
        self.branch(start_ok, span, bad);
        self.switch_to(span);
        let avail = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, blen, off.clone()));
        let fits = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, n.clone(), avail));
        self.branch(fits, ok, bad);
        self.switch_to(bad);
        self.panic_msg(outside);
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, off));
        (ptr, n)
    }

    /// The address and length (`u64`) of `xs.subarray(begin, end)`, from the array of type `t`
    /// at `place` as it is now (its header read here, at the call) and the positions evaluated
    /// before: JS's `subarray` positions, clamped to the current length.
    pub(super) fn subarray_bytes(
        &mut self,
        place: &Place,
        t: TyId,
        begin: (Operand, TyId),
        end: (Operand, TyId),
    ) -> (Operand, Operand) {
        let (data, len) = self.header_parts(place, t);
        let from = self.position(begin.0, begin.1, len.clone());
        let to = self.position(end.0, end.1, len);
        // `to - from`, or 0 when `to` is before `from`.
        let after = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Gt, to.clone(), from.clone()),
        );
        let n = Place::local(self.temp(Ty::U64));
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(after, yes, no);
        self.switch_to(yes);
        self.assign(n.clone(), Rvalue::Binary(BinOp::Sub, to, from.clone()));
        self.goto(join);
        self.switch_to(no);
        self.assign(n.clone(), Rvalue::Use(cint(0, Ty::U64)));
        self.goto(join);
        self.switch_to(join);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, from));
        (ptr, Operand::Copy(n))
    }

    /// The data pointer and the length (`u64`) in the header of the array of type `t` at
    /// `place`.
    pub(super) fn header_parts(&mut self, place: &Place, t: TyId) -> (Operand, Operand) {
        let hdr = self.content(place, t);
        let ptr = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&hdr, Proj::Field(0)))),
        );
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&hdr, Proj::Field(1)))),
        );
        (ptr, len)
    }

    /// The place of field `name` of the object of class type `t` whose pointer is at `obj`, and
    /// the field's type.
    fn class_field(&mut self, obj: &Place, t: TyId, name: &str) -> (Place, TyId) {
        let hir::TyKind::Adt(d, _) = self.cx.kind(t) else {
            ice("a view is a class")
        };
        let fields = &self.cx.adt_def(d).fields;
        let Some(i) = fields.iter().position(|f| f.name == name) else {
            ice(format_args!("a view without `{name}`"))
        };
        let ft = fields[i].ty;
        let ft = self.sub(ft);
        (self.field_place(obj, t, i as u32), ft)
    }

    /// JS's relative position `i` (of type `ty`: an `i64`, or a `usize` that is never negative)
    /// in `len` (`u64`) elements: negative counts from the end, then clamped to `0..=len`; as a
    /// `u64`.
    fn position(&mut self, i: Operand, ty: TyId, len: Operand) -> Operand {
        let out = Place::local(self.temp(Ty::U64));
        let t = self.sub(ty);
        if matches!(self.cx.kind(t), hir::TyKind::Int(k) if !k.is_signed()) {
            // `min(at, len)`.
            let over =
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Gt, i.clone(), len.clone()));
            let (cap, take, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(over, cap, take);
            self.switch_to(cap);
            self.assign(out.clone(), Rvalue::Use(len));
            self.goto(join);
            self.switch_to(take);
            self.assign(out.clone(), Rvalue::Use(i));
            self.goto(join);
            self.switch_to(join);
            return Operand::Copy(out);
        }
        let n = self.rvalue_temp(Ty::I64, Rvalue::Cast(len.clone(), Ty::I64));
        let neg = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, i.clone(), cint(0, Ty::I64)),
        );
        let (from_end, from_start, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(neg, from_end, from_start);
        // `max(len + i, 0)`.
        self.switch_to(from_end);
        let r = self.rvalue_temp(Ty::I64, Rvalue::Binary(BinOp::Add, n.clone(), i.clone()));
        let below = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, r.clone(), cint(0, Ty::I64)),
        );
        let (zero, keep) = (self.new_block(), self.new_block());
        self.branch(below, zero, keep);
        self.switch_to(zero);
        self.assign(out.clone(), Rvalue::Use(cint(0, Ty::U64)));
        self.goto(join);
        self.switch_to(keep);
        self.assign(out.clone(), Rvalue::Cast(r, Ty::U64));
        self.goto(join);
        // `min(i, len)`.
        self.switch_to(from_start);
        let over = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Gt, i.clone(), n));
        let (cap, take) = (self.new_block(), self.new_block());
        self.branch(over, cap, take);
        self.switch_to(cap);
        self.assign(out.clone(), Rvalue::Use(len));
        self.goto(join);
        self.switch_to(take);
        self.assign(out.clone(), Rvalue::Cast(i, Ty::U64));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }
}
