//! `r[k] ??= v`, `r[k] ||= v` and `r[k] &&= v` on a `Record` (also `r.name ??= v`). As in JS,
//! the record and the key are evaluated once, then the key is read, and only when the value
//! read does not decide the result is `v` evaluated and stored:
//!
//! ```text
//! { const <receiver> = r;            // only when evaluating `r` again may differ
//!   const <key> = k;                 // likewise
//!   const <old> = <receiver>[<key>]; // `V | null` on an open record: a missing key
//!   if (<old> == null / !<old> / <old>) { const <new> = v; <receiver>[<key>] = <new>; <new> }
//!   else <old> }
//! ```
//!
//! A missing key reads as `null` here (JS's `undefined`), which is falsy: `||=` stores and
//! `&&=` keeps it missing. For `??=` and `||=` the value is the stored or the kept value, so it
//! has the type `V` (TypeScript's type of the expression).

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::{block, stmt};
use super::assign_paths::is_pure_value;
use super::record::RecordKey;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, UseMode};

const KEY: &str = "<key>";
const OLD: &str = "<old>";
const NEW: &str = "<new>";

impl FnCx<'_, '_> {
    /// `object[key] op= value` (`op`: `??`, `||` or `&&`) where `obj`, the checked `object`, is
    /// a record. `as_value`: the result is used.
    #[allow(clippy::too_many_arguments)] // target parts + assignment parts
    pub(super) fn record_logical(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        key: RecordKey<'_>,
        op: ast::BinaryOp,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let (k, v) = self
            .record_args(obj.ty)
            .expect("ICE: record write on a non-record");
        let closed = self.record_keys(k).is_some();
        if let (true, RecordKey::Name(id)) = (closed, &key) {
            if !self.record_has_key(k, &id.name, id.span) {
                self.check_args_loose(std::slice::from_ref(value));
                return self.error_expr(span);
            }
        }
        if !self.require_mutable(&obj, "assign to a key of") {
            self.check_args_loose(std::slice::from_ref(value));
            return self.error_expr(span);
        }
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, Some(value), &mut stmts);
        // The key, evaluated once, before `value` (which may change what it names).
        let key_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: KEY.into(),
                span,
            }),
            span,
        );
        let key = match key {
            RecordKey::Index(e) if !(key.side_effect_free() && is_pure_value(value)) => {
                let kh = self.expr(e, Some(k), Want::Move);
                let kh = self.coerce(kh, k);
                self.bind_temp(KEY, kh, &mut stmts);
                RecordKey::Index(&key_ref)
            }
            key => key,
        };
        let old_val = self.record_read(recv.clone(), key, span);
        let old_ty = old_val.ty;
        let old = self.bind_temp(OLD, old_val, &mut stmts);
        let old_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: OLD.into(),
                span,
            }),
            span,
        );
        let cond = self.decides(op, old_ref, span);
        let unit = self.cx.ty.unit;
        let new_val = self.expr_coerce(value, v, Want::Move);
        if !as_value {
            let set = self.record_set_checked(recv, key, new_val, span);
            let then = self.mk(
                H::Block(block(
                    vec![stmt(hir::StmtKind::Expr(set), span)],
                    None,
                    span,
                )),
                unit,
                span,
            );
            let els = self.mk(H::Block(block(vec![], None, span)), unit, span);
            let kind = H::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            };
            let skip = self.mk(kind, unit, span);
            stmts.push(stmt(hir::StmtKind::Expr(skip), span));
            self.pop_scope();
            return self.mk(H::Block(block(stmts, None, span)), unit, span);
        }
        // The value: `<new>` where it was stored, else `<old>`. For `??=` and `||=` the old
        // value is non-null where it decides, so the result is a `V`; `&&=` keeps a missing
        // key's `null`.
        let ty = match op {
            ast::BinaryOp::And => old_ty,
            _ => v,
        };
        let mut set = vec![];
        let new = self.bind_temp(NEW, new_val, &mut set);
        let new_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: NEW.into(),
                span,
            }),
            span,
        );
        let call = self.record_call_keyed(recv, "__set", key, std::slice::from_ref(&new_ref), span);
        set.push(stmt(hir::StmtKind::Expr(call), span));
        let then_val = self.read_temp(new, v, span);
        let then_val = self.coerce(then_val, ty);
        let then = self.mk(H::Block(block(set, Some(then_val), span)), ty, span);
        let old_val = if ty == old_ty {
            self.read_temp(old, old_ty, span)
        } else {
            let base = self.mk(H::Local(old, UseMode::Borrow), old_ty, span);
            let mode = self.use_mode(ty, Want::Move);
            self.mk(H::UnwrapSome(Box::new(base), mode), ty, span)
        };
        let els = self.mk(H::Block(block(vec![], Some(old_val), span)), ty, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        let pick = self.mk(kind, ty, span);
        self.pop_scope();
        self.mk(H::Block(block(stmts, Some(pick), span)), ty, span)
    }
}
