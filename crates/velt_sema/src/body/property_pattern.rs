//! Object patterns that read properties rather than fields (`const { length } = xs;`, a getter
//! of a class): JavaScript's destructuring reads each key as a property, so a value whose type
//! lacks one of the pattern's keys as a field is taken apart key by key, each binding declared
//! from `value.key` as a property read would produce it.
//!
//! A value that is a variable or a field path of one is read in place for each key; any other
//! value is first held in a hidden temporary, so it is evaluated once, as in JS.

use velt_syntax::ast;

use super::pattern::BindCtx;
use super::places::{is_path, is_place};
use super::{FnCx, Want};
use crate::hir::{self, ExprKind as H, LocalId, StmtKind as S, UseMode};

impl FnCx<'_, '_> {
    /// Does destructuring a value of type `ty` with the object pattern `fields` need property
    /// reads (a key that is not a field: `length`, a getter)?
    pub(super) fn reads_properties(
        &mut self,
        ty: hir::TyId,
        fields: &[(ast::Ident, ast::Pattern)],
    ) -> bool {
        let t = &self.cx.ty;
        if t.is_bottom(ty) || t.opt_payload(ty).is_some() {
            return false;
        }
        fields
            .iter()
            .any(|(key, _)| self.field_of(ty, &key.name).is_none())
    }

    /// When the object pattern `fields` takes a method out of `init` (checked from `e`; `const
    /// { insert } = options;`), the value to take it apart from by member expressions
    /// (`pattern_defaults.rs`), so that a method is read as a function value (`method_value.rs`):
    /// `e` itself when it is a variable or a path of fields, else a hidden temporary holding
    /// `init`.
    pub(super) fn method_source(
        &mut self,
        fields: &[(ast::Ident, ast::Pattern)],
        e: &ast::Expr,
        init: hir::Expr,
        out: &mut Vec<hir::Stmt>,
    ) -> Option<ast::Expr> {
        let ty = init.ty;
        if !fields
            .iter()
            .any(|(key, _)| self.is_method_value(ty, &key.name))
        {
            return None;
        }
        if is_path(&init) {
            return Some(e.clone());
        }
        let span = init.span;
        let name = ast::Ident {
            name: format!("#d{}", self.f.locals.len()),
            span,
        };
        self.hidden_local(name.clone(), init, false, out);
        Some(ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Ident(name),
            span,
        })
    }

    /// `kind { k1: p1, k2: p2 } = init;` as `kind p1 = init.k1; kind p2 = init.k2;`.
    pub(super) fn property_decls(
        &mut self,
        kind: ast::VarKind,
        fields: &[(ast::Ident, ast::Pattern)],
        init: hir::Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        let obj = if is_path(&init) {
            init
        } else {
            let span = init.span;
            let name = ast::Ident {
                name: format!("#d{}", self.f.locals.len()),
                span,
            };
            let ty = init.ty;
            let l = self.hidden_local(name, init, false, out);
            self.mk(H::Local(l, UseMode::Borrow), ty, span)
        };
        let mutable = kind == ast::VarKind::Let;
        let union_ty = obj.ty;
        let mut bound = vec![];
        for (key, sub) in fields {
            let value = self.member_of(obj.clone(), key, Want::Borrow, key.span);
            let ctx = BindCtx::Let {
                mutable,
                place: is_place(&value),
            };
            let pat = self.pattern(sub, value.ty, ctx);
            if let hir::PatKind::Binding(l, _) = pat.kind {
                bound.push((key.name.clone(), l));
            }
            out.push(hir::Stmt {
                kind: S::LetPat { pat, init: value },
                span: sub.span,
            });
        }
        if kind == ast::VarKind::Const {
            self.note_destructured_discriminants(union_ty, &bound);
        }
    }

    /// `const { value, done } = r` with `r` of a union discriminated by the `bool` field `done`:
    /// records which other bindings a test of `done` shows to be not null, as TypeScript
    /// narrows destructured discriminated unions (issue #1114). A binding is recorded for a
    /// value of `done` when every member with that value has its field with exactly the
    /// binding's payload type (so a `T` that is itself nullable is never narrowed).
    fn note_destructured_discriminants(&mut self, u: hir::TyId, bound: &[(String, LocalId)]) {
        let Some(members) = self.cx.union_members(u) else {
            return;
        };
        for (dkey, dl) in bound {
            let Some(values) = self.cx.discriminant_values(u, dkey) else {
                continue;
            };
            let bools: Option<Vec<bool>> = values
                .iter()
                .map(|v| match v {
                    hir::LitValue::Bool(b) => Some(*b),
                    _ => None,
                })
                .collect();
            let Some(bools) = bools else {
                continue;
            };
            let mut facts = vec![];
            for (key, l) in bound.iter().filter(|(k, _)| k != dkey) {
                let Some(payload) = self.cx.ty.opt_payload(self.local_ty(*l)) else {
                    continue;
                };
                for b in [true, false] {
                    let mut with = members.iter().zip(&bools).filter(|(_, v)| **v == b);
                    let mut any = false;
                    let all = with.all(|(m, _)| {
                        any = true;
                        self.cx.field_of(*m, key).is_some_and(|(_, t)| t == payload)
                    });
                    if any && all {
                        facts.push((b, *l));
                    }
                }
            }
            if !facts.is_empty() {
                self.f.destructured_discriminants.insert(*dl, facts);
            }
        }
    }
}
