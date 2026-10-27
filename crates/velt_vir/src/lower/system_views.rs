//! Views of bytes passed to a system function's buffer parameter (#1718; sema's
//! `body/expr/system_args.rs`): the address and the length of the bytes, with no copy.
//!
//! - `xs.subarray(begin, end)` of a `u8[]` (`Intrinsic::Subarray`): the bytes between JS's
//!   `subarray` positions of `begin` and `end` (negative counts from the end, clamped).
//! - A typed array or a `DataView` (a class extending std's `BufferView`, which holds its
//!   `ArrayBuffer` `buf`, the byte offset `off` and the length `len` in elements): the bytes
//!   `off..off + byteLength` of the buffer's `u8[]`. The view's owner may have changed length
//!   since it was made: bytes outside its current length stop the program.

use velt_sema::hir::{self, Callee as HCallee, Intrinsic, TyId};

use super::operand::proj;
use super::{cint, ice, FnLower};
use crate::system::Func;
use crate::vir::{BinOp, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The std function giving a view's length in bytes (std/typed_array/core.vlt).
const VIEW_BYTES: &str = "std/typed_array/core::__viewBytes";

impl FnLower<'_, '_> {
    /// The address and length (`u64`) of the bytes of `a`, when it is a view: `None` for a
    /// `u8[]` (the caller takes its header).
    pub(super) fn system_view(
        &mut self,
        func: &Func,
        name: &str,
        a: &hir::Expr,
    ) -> Option<(Operand, Operand)> {
        if let hir::ExprKind::Call {
            callee: HCallee::Intrinsic(Intrinsic::Subarray),
            args,
        } = &a.kind
        {
            let [xs, begin, end] = args.as_slice() else {
                ice("`Subarray` takes three arguments")
            };
            let v = self.expr(xs);
            let t = self.sub(xs.ty);
            let vt = self.cx.ty(t);
            let place = self.operand_place(v, vt);
            let (data, len) = self.header_parts(&place, t);
            let from = self.position(begin, len.clone());
            let to = self.position(end, len);
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
            return Some((ptr, Operand::Copy(n)));
        }
        let t = self.sub(a.ty);
        if !self.cx.is_class(t) {
            return None;
        }
        let v = self.expr(a);
        let place = self.operand_place(v.clone(), Ty::Ptr);
        let buf = self.class_field(&place, t, "buf");
        let off = self.class_field(&place, t, "off");
        let off = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(off.0)));
        let (buf, bt) = buf;
        let (bytes, at) = self.class_field(&buf, bt, "bytes");
        let (data, len) = self.header_parts(&bytes, at);
        // `len * __size()`, through std (a typed array's element size is a method).
        let d = self
            .cx
            .prelude_def(VIEW_BYTES)
            .unwrap_or_else(|| ice("no `__viewBytes` in std/typed_array/core"));
        let f = self.cx.func_for_direct(d, vec![], self.mode);
        let n = Place::local(self.temp(Ty::U64));
        self.call(Callee::Func(f), vec![v], Some(n.clone()), false);
        let n = Operand::Copy(n);
        // Inside the owner's current length: `off + n <= len`.
        let end = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, off.clone(), n.clone()));
        let fits = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, end, len));
        let (ok, bad) = (self.new_block(), self.new_block());
        self.branch(fits, ok, bad);
        self.switch_to(bad);
        self.panic_msg(&format!(
            "system function `{}`: the bytes `{name}` views are outside its buffer, which is \
             shorter than when the view was made",
            func.name
        ));
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, off));
        Some((ptr, n))
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

    /// JS's relative position `at` (an `i64`, or a `usize` that is never negative) in `len`
    /// (`u64`) elements: negative counts from the end, then clamped to `0..=len`; as a `u64`.
    fn position(&mut self, at: &hir::Expr, len: Operand) -> Operand {
        let i = self.expr(at);
        let out = Place::local(self.temp(Ty::U64));
        let t = self.sub(at.ty);
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
