//! `...rest` in object patterns (`const { children, ...rest } = props;`, also in parameters and
//! nested patterns). A declaration whose pattern has one is taken apart through a hidden
//! temporary, as one with defaults is (`pattern_defaults.rs`); after the named keys, the rest is
//! the object of the temporary's other own properties, in the temporary's order, as in
//! JavaScript:
//!
//! - an object type, an interface of fields or a class instance: the anonymous object type of
//!   the other public fields (TypeScript's `Omit<T, "children">`; a class instance's methods and
//!   getters live on its prototype, so the rest has none), filled by the spread `{ ...#d }`,
//!   which copies an optional field only while it is present;
//! - a `Record<string, V>`: a copy of the record without the named keys
//!   (`const rest = { ...#d }; delete rest["children"];`), also a `Record<string, V>`.
//!
//! Any other type (a type parameter, an interface with methods, a record with a closed key
//! type) is an error saying it is not supported yet, and so is a class instance with a
//! TypeScript `private` field the pattern doesn't take: JS copies it into the rest, while the
//! rest's type has no such field.

use velt_common::Diagnostic;
use velt_syntax::ast::{self, Expr, ExprKind as E, Pattern, PatternKind as P, VarKind};

use super::pattern::BindCtx;
use super::pattern_defaults::{ident, mk, pat};
use super::{FnCx, Want};
use crate::hir::{self, StmtKind as S, TyKind};

/// Does `p` contain an object pattern with `...rest`?
pub(super) fn has_object_rest(p: &Pattern) -> bool {
    match &p.kind {
        P::Object { fields, rest } => {
            rest.is_some() || fields.iter().any(|(_, f)| has_object_rest(f))
        }
        P::Array { elems, .. } => elems.iter().any(has_object_rest),
        P::Default { pattern, .. } => has_object_rest(pattern),
        P::Ident(_) | P::Wildcard => false,
    }
}

impl FnCx<'_, '_> {
    /// `kind rest = <tmp without the keys taken>;` (see the module docs).
    pub(super) fn object_rest_decl(
        &mut self,
        kind: VarKind,
        rest: &ast::Ident,
        taken: &[String],
        tmp: &Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = rest.span;
        let err = self.cx.ty.error;
        let Some(ty) = self.temp_ty(tmp) else {
            return self.declare_rest(kind, rest, err, None, out);
        };
        if self.cx.ty.is_bottom(ty) {
            return self.declare_rest(kind, rest, err, None, out);
        }
        if let Some((k, _)) = self.record_args(ty) {
            if !matches!(self.cx.ty.kind(k), TyKind::Str) {
                let tn = self.cx.display(ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("`...rest` of a record with a closed key type (`{tn}`) is not supported yet"),
                        span,
                    )
                    .with_note("read the other keys by name, or type the value `Record<string, V>`"),
                );
                return self.declare_rest(kind, rest, err, None, out);
            }
            let copy = mk(E::Object(vec![ast::ObjectProp::Spread(tmp.clone())]), span);
            let v = ast::VarDecl {
                kind,
                pattern: pat(P::Ident(rest.clone()), span),
                ty: None,
                init: Some(copy),
                span,
            };
            self.var_decl(&v, span, out);
            for key in taken {
                let target = mk(
                    E::Index {
                        object: Box::new(mk(E::Ident(rest.clone()), span)),
                        index: Box::new(mk(E::Lit(ast::Lit::Str(key.clone())), span)),
                        optional: false,
                    },
                    span,
                );
                let delete = mk(
                    E::Unary {
                        op: ast::UnaryOp::Delete,
                        expr: Box::new(target),
                    },
                    span,
                );
                let stmt = ast::Stmt {
                    kind: ast::StmtKind::Expr(delete),
                    span,
                };
                self.stmt(&stmt, out);
            }
            return;
        }
        let object = match self.cx.ty.kind(ty) {
            TyKind::Adt(d, _) => self.cx.adt(*d).is_some(),
            _ => false,
        };
        if !object {
            let tn = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`...rest` of a value of type `{tn}` is not supported yet"),
                    span,
                )
                .with_note("`...rest` takes the rest of an object type, an interface of fields, a class instance or a `Record<string, V>`"),
            );
            return self.declare_rest(kind, rest, err, None, out);
        }
        if let Some(name) = self.ts_private_field(ty, taken) {
            let tn = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`...rest` of a class instance with `private` fields (`{tn}`) is not supported yet"),
                    span,
                )
                .with_note(format!(
                    "JavaScript copies `{name}` into the rest, but TypeScript leaves it out of the rest's type, which Velt cannot express yet"
                ))
                .with_note("take the fields you need by name, or make them public"),
            );
            return self.declare_rest(kind, rest, err, None, out);
        }
        let Some(mut fields) = self.cx.object_fields("...rest", ty, None, span) else {
            return self.declare_rest(kind, rest, err, None, out);
        };
        fields.retain(|f| !taken.contains(&f.name));
        let rest_ty = self.cx.anon_type_with(&fields, self.module);
        let copy = mk(E::Object(vec![ast::ObjectProp::Spread(tmp.clone())]), span);
        let init = self.expr_coerce(&copy, rest_ty, Want::Move);
        self.declare_rest(kind, rest, rest_ty, Some(init), out);
    }

    /// A TypeScript `private` field of class instance type `ty` that the pattern doesn't take
    /// (`#private` fields are not own properties in the JS sense the rest copies: they stay
    /// out, as in JS).
    fn ts_private_field(&self, ty: hir::TyId, taken: &[String]) -> Option<String> {
        let TyKind::Adt(d, _) = self.cx.ty.kind(ty) else {
            return None;
        };
        self.cx
            .adt(*d)?
            .fields
            .iter()
            .find(|f| {
                f.private_to.is_some() && !f.name.starts_with('#') && !taken.contains(&f.name)
            })
            .map(|f| f.name.clone())
    }

    /// `kind rest = init;` with `rest` of type `ty` (`init` absent after an error).
    fn declare_rest(
        &mut self,
        kind: VarKind,
        rest: &ast::Ident,
        ty: hir::TyId,
        init: Option<hir::Expr>,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = rest.span;
        let init = init.unwrap_or_else(|| self.error_expr(span));
        let ctx = BindCtx::Let {
            mutable: kind == VarKind::Let,
            place: false,
        };
        let p = pat(P::Ident(ident(&rest.name, span)), span);
        let pat = self.pattern(&p, ty, ctx);
        out.push(hir::Stmt {
            kind: S::LetPat { pat, init },
            span,
        });
    }
}
