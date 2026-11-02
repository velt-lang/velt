//! The storage floats `f16` and `bf16` in expressions (#2049): arithmetic on them runs in
//! `f32` (every operator widens a storage operand first), a literal of one is its f64 value
//! rounded once (an error where that is infinite), and only `as` narrows into one.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` widened to `f32` if it is an `f16` or `bf16` (exact), the type arithmetic on it runs
    /// in (#2049); `h` as it is otherwise.
    pub(crate) fn storage_widened(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.cx.ty.is_storage_float(h.ty) {
            return h;
        }
        let f32_ = self
            .cx
            .ty
            .primitive("f32")
            .expect("ICE: f32 is a primitive");
        let span = h.span;
        self.mk(H::Cast(Box::new(h)), f32_, span)
    }

    /// Whether a literal of type `ty` is one of a storage float, reporting it if it rounds to
    /// infinity.
    pub(super) fn storage_literal(&mut self, value: f64, ty: TyId, span: Span) -> bool {
        let storage = self.cx.ty.is_storage_float(ty);
        if storage {
            self.storage_literal_range(value, ty, span);
        }
        storage
    }

    /// Reports a literal of a storage float type (`f16`, `bf16`) that rounds to infinity
    /// (#2049): the literal is the f64 value rounded once to the type, as `as` rounds.
    pub(super) fn storage_literal_range(&mut self, value: f64, ty: TyId, span: Span) {
        use velt_common::half;
        let (bits, inf, max) = match self.cx.ty.kind(ty) {
            TyKind::Float(hir::FloatTy::F16) => {
                (half::f64_to_f16(value.to_bits()), 0x7c00, "65504")
            }
            _ => (
                half::f64_to_bf16(value.to_bits()),
                0x7f80,
                "3.3895313892515355e38",
            ),
        };
        if bits & 0x7fff == inf {
            let name = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(format!("literal out of range for `{name}`"), span).with_note(
                    format!(
                    "the literal `{}` rounds to infinity: the largest finite `{name}` is `{max}`",
                    if value.abs() >= 1e21 {
                        format!("{value:e}")
                    } else {
                        value.to_string()
                    }
                ),
                ),
            );
        }
    }

    /// `x as f16` on an `any` (or `unknown`): not supported yet (the `any` part of #2049 checks
    /// for a number and rounds).
    pub(super) fn any_as_storage(&mut self, target: TyId, span: Span) -> hir::Expr {
        let d = self.cx.display(target);
        let msg = format!("an `any` doesn't convert to `{d}` yet: write `(x as number) as {d}`");
        self.cx.err(msg, span);
        self.error_expr(span)
    }

    /// A bitwise operator with an `f16`/`bf16` operand: an error (#2049), reported before
    /// the JS ToInt32 path that `number` operands take.
    pub(super) fn storage_bitwise(
        &mut self,
        op: ast::BinaryOp,
        l: &hir::Expr,
        r: &hir::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        use ast::BinaryOp as B;
        let bitwise = matches!(
            op,
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr | B::UShr
        );
        let h = [l, r]
            .into_iter()
            .find(|h| bitwise && self.cx.ty.is_storage_float(h.ty))?;
        let t = self.cx.display(h.ty);
        self.cx.error(
            Diagnostic::error(format!("bitwise operators take integers, not `{t}`"), span)
                .with_note(format!(
                    "an `{t}` is a float: convert it to an integer type with `as` first"
                )),
        );
        Some(self.error_expr(span))
    }
}
