//! Emitting calls to the runtime string builder (`velt_rt_strbuf_*`, rt_abi_async.md §12.1),
//! shared by `JSON.stringify`, the format glue behind `console.log`/`${x}` and template literals.
//! A builder has the `VeltStr` layout, so a filled builder local *is* the owned result string.

use velt_sema::hir::{TyId, TyKind};

use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, Const, Operand, Place, Ty, STR_AGG};

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

    /// Append the string at address `s` to the builder at `buf`.
    pub(super) fn push_str(&mut self, buf: &Operand, s: Operand) {
        self.call_rt(Rt::StrbufPushStr, vec![buf.clone(), s], None);
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
