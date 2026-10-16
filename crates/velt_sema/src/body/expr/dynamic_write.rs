//! Member writes on an `any` (phase 1b of #924; hir_encodings.md "any and unknown"):
//! `x.f = v`, `x["f"] = v` and `x[k] = v` become `Intrinsic::AnySet` / `AnySetIndex`, which
//! lowering sends through the descriptor of the value held. The value is boxed; the slot
//! converts it to the member's type (a `TypeError` when it is of another type) or passes it to
//! the member's setter. On an `unknown` they are errors until it is narrowed, as in TypeScript.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, UseMode};

/// The key of a dynamic write.
enum WriteKey {
    /// `x.f` / `x["f"]`, or a `string` / `any` key.
    Str(hir::Expr),
    /// A number key.
    Num(hir::Expr),
}

impl FnCx<'_, '_> {
    /// The receiver of an assignment `obj.f = v` / `obj[k] = v`: `Err(obj)` when it is not
    /// dynamic, `Ok(Some(obj))` for an `any`, `Ok(None)` for an `unknown` (reported).
    pub(crate) fn dynamic_write_receiver(
        &mut self,
        obj: hir::Expr,
    ) -> Result<Option<hir::Expr>, hir::Expr> {
        match self.cx.dynamic(obj.ty) {
            None => Err(obj),
            Some(Dynamic::Unknown) => {
                self.unknown_use_error(obj.span);
                Ok(None)
            }
            Some(Dynamic::Any) => {
                let mut obj = obj;
                set_place_mode(&mut obj, UseMode::Borrow);
                self.cx.note_dyn_members();
                Ok(Some(obj))
            }
        }
    }

    /// `obj.f = v` / `obj[k] = v` (`target`) on an `any` `obj`. With `as_value`, the
    /// expression is also the value assigned (returned `true`), as in JavaScript (never the
    /// member read back, which might run a getter).
    pub(crate) fn dynamic_assign(
        &mut self,
        obj: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> (hir::Expr, bool) {
        let Some(any) = self.any_ty() else {
            return (self.error_expr(span), false);
        };
        if op.is_some() {
            self.cx.error(
                Diagnostic::error(
                    "a compound assignment to a member of an `any` is not supported yet (#924)",
                    span,
                )
                .with_note(
                    "convert the member first and assign the result: `x.n = (x.n as number) + 1`",
                ),
            );
            self.expr(value, None, Want::Borrow);
            return (self.error_expr(span), false);
        }
        let key = match &strip(target).kind {
            ast::ExprKind::Member { prop, .. } => {
                self.note_dynamic_write(&prop.name, prop.span);
                let ty = self.cx.ty.str_;
                WriteKey::Str(self.mk(H::Lit(Lit::Str(prop.name.clone())), ty, prop.span))
            }
            ast::ExprKind::Index { index, .. } => match self.dynamic_write_key(index) {
                Some(k) => k,
                None => {
                    self.expr(value, None, Want::Borrow);
                    return (self.error_expr(span), false);
                }
            },
            _ => return (self.error_expr(span), false),
        };
        let v = self.expr(value, Some(any), Want::Move);
        if self.cx.ty.is_bottom(v.ty) {
            return (self.error_expr(span), false);
        }
        let v = self.coerce(v, any);
        let unit = self.cx.ty.unit;
        let (i, key) = match key {
            WriteKey::Str(k) => (Intrinsic::AnySet, k),
            WriteKey::Num(k) => (Intrinsic::AnySetIndex, k),
        };
        if !as_value {
            return (self.intrinsic(i, vec![obj, key, v], unit, span), false);
        }
        // `{ const t = v; x.f = share(t); t }`.
        let tmp = self.new_local("<assigned>", any, false, span, LocalKind::Temp);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), any, span);
        let share = self.intrinsic(Intrinsic::Share, vec![read], any, span);
        let set = self.intrinsic(i, vec![obj, key, share], unit, span);
        let result = self.mk(H::Local(tmp, UseMode::Move), any, span);
        let block = hir::Block {
            stmts: vec![
                hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local: tmp,
                        init: Some(v),
                    },
                    span,
                },
                hir::Stmt {
                    kind: hir::StmtKind::Expr(set),
                    span,
                },
            ],
            value: Some(Box::new(result)),
            span,
        };
        (self.mk(H::Block(block), any, span), true)
    }

    /// The key of `x[index] = v` on an `any`: a string or `any` key names a member, a number
    /// key an element (`None` after reporting an error).
    fn dynamic_write_key(&mut self, index: &ast::Expr) -> Option<WriteKey> {
        let k = self.expr(index, None, Want::Borrow);
        if self.cx.ty.is_bottom(k.ty) {
            return None;
        }
        let str_ = self.cx.ty.str_;
        let f64_ = self.cx.ty.f64;
        let kt = self.cx.widened(k.ty);
        if let H::Lit(Lit::Str(name)) = &k.kind {
            let name = name.clone();
            self.note_dynamic_write(&name, index.span);
        } else {
            // A key known only at run time may name any field.
            self.note_dynamic_write("", index.span);
        }
        if self.cx.dynamic(k.ty) == Some(Dynamic::Any) || kt == str_ {
            let k = if kt == str_ { self.coerce(k, str_) } else { k };
            return Some(WriteKey::Str(k));
        }
        if self.cx.ty.is_numeric(kt) {
            return Some(WriteKey::Num(self.coerce_number(k, f64_)));
        }
        let tn = self.cx.display(k.ty);
        self.cx.error(
            Diagnostic::error(
                format!("indexing an `any` with a `{tn}` key is not supported yet (#924)"),
                index.span,
            )
            .with_note("use a `string` or `number` key"),
        );
        None
    }

    /// A write through `any` to field `name` (empty: a key known only at run time, which may
    /// be any field) of whatever object it holds, for the copies of `crate::object_copies`.
    fn note_dynamic_write(&mut self, name: &str, span: Span) {
        self.cx
            .object_copies
            .writes
            .push(crate::object_copies::Write {
                ty: None,
                name: name.to_string(),
                span,
            });
    }

    /// `x.f++`, `x.f.push(v)` and other writes through a member of an `any` that are not an
    /// assignment.
    pub(super) fn dynamic_write_error(&mut self, span: Span) {
        self.cx.error(
            Diagnostic::error(
                "changing a member of an `any` in place is not supported yet (#924)",
                span,
            )
            .with_note("assign it instead (`x.f = v`), or convert the value to its type first (`const o = x as T;`)"),
        );
    }
}

fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip(inner),
        _ => e,
    }
}
