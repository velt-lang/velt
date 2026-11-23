//! `Object.isFrozen(x)` (`crate::freezable`, #2623): `true` for a primitive, as in Node, and for
//! a value of a type `Object.freeze` made of a literal (readonly fields); the value's frozen
//! mark for an object type (`Intrinsic::IsFrozen`, which makes the types frozen values of reach
//! it freezable); `false` for other values, which Velt can't freeze. `Object.isSealed` answers
//! the same, and `Object.isExtensible` the opposite: `Object.freeze` is the only way a Velt
//! program seals an object or stops it being extended (`seal` and `preventExtensions` are not
//! supported yet).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, TyKind};

impl FnCx<'_, '_> {
    /// `Object.isFrozen(arg)`, `Object.isSealed(arg)` (`name`), or the negation of either,
    /// `Object.isExtensible(arg)` (`negate`; module docs).
    pub(super) fn object_is_frozen(
        &mut self,
        name: &str,
        negate: bool,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let [arg] = args else {
            return self.arity_error(&format!("Object.{name}"), 1, args, span);
        };
        let h = self.expr(arg, None, Want::Borrow);
        if self.cx.ty.is_bottom(h.ty) {
            return self.error_expr(span);
        }
        let bool_ = self.cx.ty.bool_;
        let t = h.ty;
        let frozen_literal = matches!(self.cx.ty.kind(t), TyKind::Adt(d, _) if self.cx.objects.frozen_defs.contains(d));
        let object = matches!(self.cx.ty.kind(t), TyKind::Adt(..)) && self.cx.is_object_type(t);
        let answer = match () {
            _ if frozen_literal => Some(true),
            _ if object => None,
            _ if self.cx.ty.opt_payload(t).is_some() || self.cx.union_members(t).is_some() => {
                let shown = self.cx.display(t);
                self.cx.error(
                    Diagnostic::error(
                        format!("`Object.{name}` of a `{shown}` is not supported yet"),
                        arg.span,
                    )
                    .with_note(
                        "narrow the value to one object type first (`if (x !== undefined)`)",
                    ),
                );
                return self.error_expr(span);
            }
            _ => Some(!self.may_hold_object(t)),
        };
        match answer {
            Some(b) => {
                let value = self.mk(H::Lit(hir::Lit::Bool(b != negate)), bool_, span);
                let block = hir::Block {
                    stmts: vec![hir::Stmt {
                        kind: hir::StmtKind::Expr(h),
                        span,
                    }],
                    value: Some(Box::new(value)),
                    span,
                };
                self.mk(H::Block(block), bool_, span)
            }
            None => {
                self.cx.objects.is_frozen_types.push(t);
                let frozen = self.intrinsic(Intrinsic::IsFrozen, vec![h], bool_, span);
                match negate {
                    true => self.mk(
                        H::Unary {
                            op: hir::UnOp::Not,
                            expr: Box::new(frozen),
                        },
                        bool_,
                        span,
                    ),
                    false => frozen,
                }
            }
        }
    }

    /// Is a value of type `t` an object in JavaScript (not a primitive): a class instance,
    /// an array, a function, a collection, an object type?
    fn may_hold_object(&mut self, t: hir::TyId) -> bool {
        !matches!(
            self.cx.ty.kind(t),
            TyKind::Bool
                | TyKind::Str
                | TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Symbol
                | TyKind::Unit
                | TyKind::Literal(_)
                | TyKind::Simd(_)
        )
    }
}
