//! The read-modify-write of an asserted target (`crate::asserted_targets`): `e! op= v`, `e!++`
//! and `--e!` where `e`'s type has `null` or `undefined` in it (an optional field, a
//! `T | undefined` variable, a key of a `Record<string, V>`). As in TypeScript, the target is
//! read as `e!` is: the value is checked to be present, the new value is computed from it, and
//! written back. The target's object and indices are evaluated once, before the right-hand side
//! (a getter on its path runs once):
//!
//! ```text
//! { let <object> = …; let <index> = …;   // objects that are not places, and indices
//!   let <old> = e!;                      // the read, checked
//!   let <new> = <old> op v;              // or <old> ± 1
//!   e = <new>;
//!   <new> or <old> }                     // the value, where one is wanted
//! ```
//!
//! On an `f16` / `bf16` target the new value is a `number`, rounded once on the store, and the
//! value of the assignment is the unrounded one, as for `e op= v` (`storage_floats.rs`).
//!
//! A target whose type has neither `null` nor `undefined` in it is an ordinary compound
//! assignment (the assertion has no effect there), and so are the targets with rules of their
//! own (indexers, `any` receivers); a setter's is in `accessor_rmw`.

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::{binary, Rmw};
use super::record::{record_parts, RecordKey};
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, LocalId, TyId};

const OLD: &str = "<asserted old>";
const NEW: &str = "<asserted new>";
const KEY: &str = "<asserted key>";
const STORE: &str = "<asserted store>";
/// The message of a failed assertion, as for `e!` elsewhere (`matching::non_null`).
pub(super) const ASSERT_MSG: &str = "non-null assertion failed: the value is null";

/// The temporaries of an asserted read-modify-write.
struct Bound {
    old: LocalId,
    old_ty: TyId,
    new: LocalId,
    new_ty: TyId,
}

impl FnCx<'_, '_> {
    /// Is the compound assignment or update being checked one whose target was asserted, and
    /// does the target's type, `ty`, have `null` or `undefined` in it? The mark is used up
    /// either way.
    pub(super) fn take_asserted(&mut self, ty: TyId) -> bool {
        std::mem::take(&mut self.asserted_target) && self.cx.ty.opt_payload(ty).is_some()
    }

    /// `f` with the mark of an asserted target set for the compound assignment or update at
    /// `span` (`Ctx::asserted_rmw`), and the outer one back after it.
    pub(super) fn with_asserted<R>(
        &mut self,
        rmw: bool,
        span: Span,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let marked = rmw && self.cx.asserted_rmw.contains(&span);
        let outer = std::mem::replace(&mut self.asserted_target, marked);
        let r = f(self);
        self.asserted_target = outer;
        r
    }

    /// `place op= v` / `place++` on the writable `place` of an asserted target.
    pub(super) fn asserted_place_rmw(
        &mut self,
        place: hir::Expr,
        rmw: Rmw,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let mut place = place;
        self.hoist_indices(&mut place, &mut stmts);
        let read = self.place_read(&place, Want::Borrow);
        let Some(b) = self.asserted_values(read, &rmw, &mut stmts, span) else {
            self.pop_scope();
            return self.error_expr(span);
        };
        let reread = place.clone();
        let new = self.read_temp(b.new, b.new_ty, span);
        let new = self.storage_rounded(new, place.ty);
        let value = self.coerce(new, place.ty);
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(value),
        };
        let unit = self.cx.ty.unit;
        let assign = self.mk(kind, unit, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(assign),
            span,
        });
        let value = match as_value {
            true => {
                Some(self.asserted_result(&b, &rmw, |s| s.place_read(&reread, Want::Borrow), span))
            }
            false => None,
        };
        self.pop_scope();
        self.asserted_block(stmts, value, span)
    }

    /// `r[k] op= v` / `r.name++` on a key of an open record `obj` (`target`) that was asserted.
    /// `Err(obj)` when the record's keys are always present (an ordinary compound assignment).
    pub(super) fn asserted_record_rmw(
        &mut self,
        obj: hir::Expr,
        target: &ast::Expr,
        rmw: Rmw,
        as_value: bool,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let (object, key) = record_parts(target);
        let open = match self.record_args(obj.ty) {
            Some((k, _)) => self.record_keys(k).is_none() && !self.record_view_key(obj.ty, &key),
            None => false,
        };
        if !std::mem::take(&mut self.asserted_target) || !open {
            return Err(obj);
        }
        let mut stmts = vec![];
        self.push_scope();
        let mut obj = obj;
        self.hoist_object(&mut obj, &mut stmts);
        // A key that is not a literal is evaluated once, before the right-hand side.
        let key_ref = ident(KEY, span);
        let key = match key {
            RecordKey::Index(e) | RecordKey::Computed(e) if !is_lit(e) => {
                let h = self.expr(e, None, Want::Borrow);
                self.bind_temp(KEY, h, &mut stmts);
                RecordKey::Index(&key_ref)
            }
            key => key,
        };
        let read = self.record_read(obj.clone(), key, span);
        let Some(b) = self.asserted_values(read, &rmw, &mut stmts, span) else {
            self.pop_scope();
            return Ok(self.error_expr(span));
        };
        // An `f16` / `bf16` value is stored rounded (the value of the assignment is not).
        let new_ref = match self.record_args(obj.ty) {
            Some((_, v)) if self.cx.ty.is_storage_float(v) => {
                let new = self.read_temp(b.new, b.new_ty, span);
                let rounded = self.storage_rounded(new, v);
                self.bind_temp(STORE, rounded, &mut stmts);
                ident(STORE, span)
            }
            _ => ident(NEW, span),
        };
        let reread = obj.clone();
        let set = self.record_assign(obj, object, key, None, target, &new_ref, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(set),
            span,
        });
        let value = match as_value {
            true => {
                let read = |s: &mut Self| s.record_read(reread, key, span);
                Some(self.asserted_result(&b, &rmw, read, span))
            }
            false => None,
        };
        self.pop_scope();
        Ok(self.asserted_block(stmts, value, span))
    }

    /// The old value `read` checked and bound, and the new one computed from it and bound
    /// (`None` after an error).
    fn asserted_values(
        &mut self,
        read: hir::Expr,
        rmw: &Rmw,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> Option<Bound> {
        let old_val = self.checked_present(read, ASSERT_MSG, None, true, span);
        let old_ty = old_val.ty;
        let new_ast = match *rmw {
            Rmw::Compound(op, v) => binary(op, ident(OLD, span), v.clone(), span),
            Rmw::Update(op, _) => {
                let numeric = self.cx.ty.is_numeric(old_ty) || self.cx.ty.is_storage_float(old_ty);
                if !numeric && !self.operator_type(old_ty) {
                    if !self.cx.ty.is_bottom(old_ty) {
                        let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
                        let tn = self.cx.display(old_ty);
                        self.cx
                            .err(format!("cannot apply `{opname}` to type `{tn}`"), span);
                    }
                    return None;
                }
                let one = synth(
                    ast::ExprKind::Lit(ast::Lit::Int {
                        value: 1,
                        suffix: None,
                    }),
                    span,
                );
                let op = match op {
                    ast::UpdateOp::Inc => ast::BinaryOp::Add,
                    ast::UpdateOp::Dec => ast::BinaryOp::Sub,
                };
                binary(op, ident(OLD, span), one, span)
            }
            Rmw::Logical(..) => panic!("ICE: a logical assignment is not an asserted read"),
        };
        let old = self.bind_temp(OLD, old_val, stmts);
        let new_val = self.expr(&new_ast, Some(old_ty), Want::Move);
        let new_ty = new_val.ty;
        let new = self.bind_temp(NEW, new_val, stmts);
        Some(Bound {
            old,
            old_ty,
            new,
            new_ty,
        })
    }

    /// The value of an asserted read-modify-write: the old value for a postfix update, else
    /// the new one (read again through `reread` when it is not copied).
    fn asserted_result(
        &mut self,
        b: &Bound,
        rmw: &Rmw,
        reread: impl FnOnce(&mut Self) -> hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if matches!(rmw, Rmw::Update(_, false)) {
            return self.read_temp(b.old, b.old_ty, span);
        }
        if self.cx.is_copy(b.new_ty) {
            return self.read_temp(b.new, b.new_ty, span);
        }
        let read = reread(self);
        self.checked_present(read, ASSERT_MSG, None, false, span)
    }

    /// The new value `new` (a `number` on a storage float) rounded once to the `f16` / `bf16`
    /// that `ty` (the target's type, nullable) holds; `new` as it is on other targets.
    fn storage_rounded(&mut self, new: hir::Expr, ty: TyId) -> hir::Expr {
        let held = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        if !self.cx.ty.is_storage_float(held) || new.ty == held {
            return new;
        }
        let span = new.span;
        self.mk(H::Cast(Box::new(new)), held, span)
    }

    fn asserted_block(
        &mut self,
        stmts: Vec<hir::Stmt>,
        value: Option<hir::Expr>,
        span: Span,
    ) -> hir::Expr {
        let ty = value.as_ref().map_or(self.cx.ty.unit, |v| v.ty);
        let block = hir::Block {
            stmts,
            value: value.map(Box::new),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}

fn ident(name: &str, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Ident(ast::Ident {
            name: name.into(),
            span,
        }),
        span,
    )
}

fn is_lit(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Paren(x) => is_lit(x),
        _ => false,
    }
}
