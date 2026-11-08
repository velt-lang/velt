//! `ToString(x)` of one value (moved out of `template.rs`): a scalar's text through std's text
//! hooks, a clone of a string, an option of one through option_text.rs, and the format glue into
//! a builder for everything else.

use velt_sema::hir::{self, TyId, TyKind};

use super::hook_calls::{self as hooks, ty, Construct, HookTy};
use super::rt::Rt;
use super::FnLower;
use crate::vir::{Operand, Place, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

impl FnLower<'_, '_> {
    /// `ToString(x)` → owned string: `velt_rt_str_from_*` for scalars, a clone for strings, the
    /// format glue into a builder for everything else.
    /// `implicit`: a `TemplateToString`, where a symbol throws.
    pub(super) fn stringify(&mut self, a: &hir::Expr, ty: TyId, implicit: bool) -> Operand {
        let aty = self.sub(a.ty);
        let ty = self.sub(ty);
        let kind = self.cx.kind(aty);
        // An option of a float, boolean or string: its payload's text or the nothing's word
        // (option_text.rs). An integer payload goes through the builder, whose hooks it has.
        let option = self
            .option_scalar(aty)
            .filter(|(p, _)| !matches!(self.cx.kind(*p), TyKind::Int(_)));
        if option.is_none()
            && !matches!(
                kind,
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
            )
        {
            let (buf, bp) = self.new_strbuf(32);
            self.own_temp(buf, ty);
            self.push_formatted(&bp, a, implicit);
            return Operand::Copy(Place::local(buf));
        }
        let v = self.expr(a);
        let out = self.temp(STR);
        match option {
            Some(pn) => self.option_text_into(v, aty, pn, out),
            None => self.scalar_text_into(v, aty, out),
        }
        self.own_temp(out, ty);
        Operand::Copy(Place::local(out))
    }

    /// The text of the number, boolean or string `v` (of type `aty`) into the string local
    /// `out`, as `stringify` makes it.
    pub(super) fn scalar_text_into(&mut self, v: Operand, aty: TyId, out: crate::vir::Local) {
        let o = self.addr(Place::local(out));
        let from = self.cx.ty(aty);
        let kind = self.cx.kind(aty);
        match kind {
            // std's number printing, as `push_scalar` (strbuf.rs) appends it.
            TyKind::Int(it) if it.is_signed() => {
                let v = self.cast_to(v, from, Ty::I64);
                hooks::int_text(self, Construct::TextInt, ty::I64::arg(v))
                    .emit_to(self, Place::local(out));
            }
            TyKind::Int(_) => {
                let v = self.cast_to(v, from, Ty::U64);
                hooks::uint_text(self, Construct::TextUint, ty::U64::arg(v))
                    .emit_to(self, Place::local(out));
            }
            TyKind::Float(_) => {
                let v = self.float_as_f64(v, aty);
                let call = hooks::float_text(self, ty::F64::arg(v.clone()))
                    .or_else(|| hooks::number_text(self, ty::F64::arg(v.clone())));
                match call {
                    Some(c) => c.emit_to(self, Place::local(out)),
                    None => self.call_rt(Rt::StrFromF64, vec![v, o], None),
                }
            }
            TyKind::Bool => self.call_rt(Rt::StrFromBool, vec![v, o], None),
            _ => {
                let src = self.operand_addr(v, STR);
                self.call_rt(Rt::StrClone, vec![src, o], None);
            }
        }
    }
}
