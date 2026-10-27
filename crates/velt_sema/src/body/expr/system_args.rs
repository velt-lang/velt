//! Arguments of a `declare system function`'s byte buffer parameters (`u8[]`, `readonly u8[]`;
//! docs/reference/system-functions.md). Besides a `u8[]`, such a parameter takes a view of bytes
//! (#1718):
//!
//! - a typed array or a `DataView` (a class extending std's `BufferView`), passed as it is:
//!   lowering passes the address and length of the bytes it views;
//! - `xs.subarray(begin, end)` of a `u8[]` `xs`, which becomes `Intrinsic::Subarray`: those
//!   bytes of `xs`, borrowed for the call, with no copy and no object (`Buffer.subarray` in
//!   Node).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::method_value::strip_parens;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, IntTy, Intrinsic, TyId, TyKind};

/// The class every typed array and `DataView` extends (std/typed_array/core.vlt).
const BUFFER_VIEW: &str = "std/typed_array/core::BufferView";

impl FnCx<'_, '_> {
    /// Is `t` `u8[]`?
    pub(crate) fn is_byte_array(&self, t: TyId) -> bool {
        self.cx
            .ty
            .array_elem(t)
            .is_some_and(|e| matches!(self.cx.ty.kind(e), TyKind::Int(IntTy::U8)))
    }

    /// Is `t` a view of bytes: a class extending std's `BufferView` (a typed array, a
    /// `DataView`)?
    pub(crate) fn is_byte_view(&self, t: TyId) -> bool {
        let mut cur = self.cx.class_of(t).map(|(d, _)| d);
        for _ in 0..64 {
            let Some(a) = cur.and_then(|d| self.cx.adt(d)) else {
                return false;
            };
            if a.qual_name == BUFFER_VIEW {
                return true;
            }
            cur = a.base.and_then(|b| self.cx.class_of(b)).map(|(d, _)| d);
        }
        false
    }

    /// The argument `e` of a system function's parameter of type `pty`, when it is a byte
    /// parameter and `e` is a view: `xs.subarray(...)` (of a `u8[]`: `Intrinsic::Subarray`; of
    /// a typed array: its method), or a value of a view type. `None` leaves `e` to the ordinary
    /// checks (a `u8[]`, an array literal, anything else).
    pub(super) fn system_view_arg(
        &mut self,
        e: &ast::Expr,
        pty: TyId,
        want: Want,
    ) -> Option<hir::Expr> {
        if !self.is_byte_array(pty) {
            return None;
        }
        let inner = strip_parens(e);
        if let ast::ExprKind::Call {
            callee,
            type_args,
            args,
            optional: false,
        } = &inner.kind
        {
            if let ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } = &callee.kind
            {
                if prop.name == "subarray" {
                    let mut recv = self.expr(object, None, want);
                    if !self.is_byte_array(recv.ty) {
                        let call = self.method_call_on(recv, prop, type_args, args, None, e.span);
                        return Some(call);
                    }
                    if want == Want::BorrowMut {
                        self.use_mutably(&mut recv, "modify");
                    }
                    return Some(self.subarray_of(recv, args, e.span));
                }
            }
        }
        // A literal takes its element type from the parameter.
        if matches!(inner.kind, ast::ExprKind::Array(_)) {
            return None;
        }
        let h = self.expr(e, None, want);
        Some(h)
    }

    /// `xs.subarray(begin, end)` of the `u8[]` `xs` (checked): JS numbers or integers, as
    /// `i64` (a float converted as a saturating cast: truncated, NaN 0), or as `usize` for an
    /// unsigned integer (never negative); `end` defaults to the end.
    fn subarray_of(&mut self, xs: hir::Expr, args: &[ast::Expr], span: Span) -> hir::Expr {
        let (i64_, usize_) = (self.cx.ty.i64, self.cx.ty.usize);
        if args.len() > 2 {
            let msg = format!("`subarray` takes 2 arguments, not {}", args.len());
            self.cx.error(Diagnostic::error(msg, span));
        }
        let mut bounds = vec![];
        for a in args.iter().take(2) {
            let h = self.expr(a, None, Want::Borrow);
            let unsigned = matches!(self.cx.ty.kind(h.ty), TyKind::Int(i) if !i.is_signed());
            let to = if unsigned { usize_ } else { i64_ };
            let h = if h.ty == to {
                h
            } else if self.cx.ty.is_int(h.ty) || self.cx.ty.is_float(h.ty) {
                let at = h.span;
                self.mk(H::Cast(Box::new(h)), to, at)
            } else {
                let tn = self.cx.display(h.ty);
                self.cx.error(Diagnostic::error(
                    format!("a position of `subarray` is a number, not `{tn}`"),
                    h.span,
                ));
                self.error_expr(h.span)
            };
            bounds.push(h);
        }
        let ends = [0u128, i64::MAX as u128];
        while bounds.len() < 2 {
            let v = ends[bounds.len()];
            bounds.push(self.mk(H::Lit(hir::Lit::Int(v)), i64_, span));
        }
        let ty = xs.ty;
        let mut all = vec![xs];
        all.extend(bounds);
        self.intrinsic(Intrinsic::Subarray, all, ty, span)
    }
}
