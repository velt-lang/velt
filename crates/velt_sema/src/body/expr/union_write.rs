//! Writing a member every member of a union has (#379): `u.value = v` for a
//! `u: HTMLInputElement | HTMLTextAreaElement` (sigx's patchProp), which TypeScript allows when
//! each member has a writable `value` of the type `v` converts to. It is a match on the member,
//! each arm writing the field of the member it holds (through the interface's setter, or the
//! object's own field), in Node's order: `u` first (held when `v` could change what it names),
//! then `v`, once, then the write into the object `u` gave. Afterwards nothing is known of
//! `u.value` any more, as after any assignment.
//!
//! Each arm's write is an ordinary assignment to the member bound in it (`<m>.value = <v>`), so it
//! is checked and lowered as one. Only a field every member has, writable (not `readonly`, not a
//! getter alone), of one type, and only a plain `=`; anything else keeps the error a write
//! through a union value has (`discriminated.rs` `union_field`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, TyKind, UseMode};

use super::object_builtins::strip_parens;

impl FnCx<'_, '_> {
    /// `target = value` when `target` is `<u>.prop` on a union whose members all have a
    /// writable field `prop` of one type (module docs); `None` otherwise.
    pub(super) fn union_member_write(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        if op.is_some() {
            return None;
        }
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &strip_parens(target).kind
        else {
            return None;
        };
        let uty = self.peek_union_ty(object)?;
        let name = &prop.name;
        self.common_writable_field(uty, name)?;
        // As in Node: the object first (held when the value could change what it names), then
        // the value, once, then the write into the object the first step gave.
        let mut stmts = vec![];
        let obj = self.expr(object, None, Want::Borrow);
        let obj = self.receiver_once(obj, object, Some(value), &mut stmts);
        let Some(fty) = self.common_writable_field(obj.ty, name) else {
            // The checked object's type differs from the peek's (a read that narrows): the error
            // a write through a union value has.
            let un = self.cx.display(obj.ty);
            self.cx.err(
                format!("cannot assign `{name}` of a union value `{un}`"),
                prop.span,
            );
            self.expr(value, None, Want::Borrow);
            return Some(self.error_expr(span));
        };
        let v = self.expr_coerce(value, fty, Want::Move);
        let tmp_name = format!("<union write {}>", span.lo);
        let tmp = self.hidden_name(&tmp_name, fty, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(v),
            },
            span,
        });
        let def = self.cx.union_def(obj.ty)?.0;
        let members = self.cx.union_members(obj.ty).unwrap_or_default();
        let mode = match is_place(&obj) {
            true => UseMode::Borrow,
            false => UseMode::Move,
        };
        let mut arms = vec![];
        for (v, m) in members.into_iter().enumerate() {
            let bound = format!("<union member {} {v}>", span.lo);
            let b = self.hidden_name(&bound, m, span);
            let write = synth(
                ast::ExprKind::Assign {
                    op: None,
                    target: Box::new(synth(
                        ast::ExprKind::Member {
                            object: Box::new(ident(&bound, target.span)),
                            prop: prop.clone(),
                            optional: false,
                        },
                        target.span,
                    )),
                    value: Box::new(ident(&tmp_name, value.span)),
                },
                span,
            );
            let body = self.expr(&write, None, Want::Borrow);
            let bind = hir::Pat {
                kind: P::Binding(b, mode),
                ty: m,
                span,
            };
            arms.push(hir::Arm {
                pat: hir::Pat {
                    kind: P::Variant {
                        def,
                        variant: v as u32,
                        args: vec![bind],
                    },
                    ty: obj.ty,
                    span,
                },
                guard: None,
                body,
            });
        }
        // What was known of `u.prop` before no longer holds (as after any assignment).
        self.unnarrow_fields(target);
        let unit = self.cx.ty.unit;
        let kind = H::Match {
            scrutinee: Box::new(obj),
            arms,
        };
        let m = self.mk(kind, unit, span);
        Some(self.with_lets(stmts, m))
    }

    /// A local named `name` in the innermost scope, so synthesized code can name it.
    fn hidden_name(&mut self, name: &str, ty: TyId, span: Span) -> crate::hir::LocalId {
        let l = self.new_local(name, ty, false, span, LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(name.to_string(), l);
        l
    }

    /// The type of `e` when it is a variable, a path of fields or a cast of one, and that type is
    /// a union, checked on trial (everything the check records is rolled back; `e` is checked
    /// for real where it is used).
    fn peek_union_ty(&mut self, e: &ast::Expr) -> Option<TyId> {
        let inner = match &strip_parens(e).kind {
            ast::ExprKind::Cast { expr, .. } => expr,
            _ => e,
        };
        if !super::method_value::is_path(inner) {
            return None;
        }
        let ty = self.trial_with(|s| s.expr(e, None, Want::Borrow).ty)?;
        self.cx.union_def(ty).is_some().then_some(ty)
    }

    /// The type of the writable field `name` every member of union `u` has, when it is one
    /// type: an interface's with a setter, or an object type's or class's own, not `readonly`.
    fn common_writable_field(&mut self, u: TyId, name: &str) -> Option<TyId> {
        let mut ty = None;
        for m in self.cx.union_members(u)? {
            let fty = match self.cx.ty.kind(m).clone() {
                TyKind::Dyn(..) => {
                    let f = self.iface_field_of(m, name)?;
                    if f.readonly || self.cx.setter_slot(f.iface, f.index).is_none() {
                        return None;
                    }
                    f.ty
                }
                TyKind::Adt(d, _) => {
                    let (i, fty) = self.cx.field_seen_from(m, name, None)?;
                    if self.cx.adt(d)?.fields.get(i as usize)?.readonly {
                        return None;
                    }
                    fty
                }
                _ => return None,
            };
            if ty.is_some_and(|t| t != fty) {
                return None;
            }
            ty = Some(fty);
        }
        ty
    }
}

/// A synthesized expression of `kind`.
fn synth(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        kind,
        span,
        id: ast::NodeId(u32::MAX),
    }
}

/// The variable `name`.
fn ident(name: &str, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }),
        span,
    )
}
