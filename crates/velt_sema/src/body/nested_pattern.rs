//! Nested array patterns over iterables (`const [[p, q], [r]] = [gen(), gen()]`,
//! `for (const [k, [a, b]] of pairs)`): `consume.rs` `destructured` turns the value a
//! *top-level* array pattern takes apart into an array; a nested array pattern whose value is an
//! iterable (not an array or tuple) is split off here. The outer pattern binds that value to a
//! hidden local, and a destructuring declaration of the hidden local follows, which takes the
//! values it needs in turn:
//!
//! ```text
//! const [[p, q], [r]] = [gen(), gen()];
//!   →  const [<nested#N.0>, <nested#N.1>] = [gen(), gen()];
//!      const [p, q] = <nested#N.0>;
//!      const [r] = <nested#N.1>;
//! ```
//!
//! JS takes the inner values as it reaches each nested pattern; here every value of the outer
//! pattern is taken first (iteration.md "Deviations": only an outer iterable whose generator
//! has effects can tell).

use velt_syntax::ast;

use super::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// `p` (a pattern over values of type `ty`) with its nested array patterns over iterables
    /// replaced by hidden names, and those (name, pattern) pairs; `None` when it has none.
    pub(super) fn split_nested(
        &mut self,
        p: &ast::Pattern,
        ty: TyId,
    ) -> Option<(ast::Pattern, Vec<(String, ast::Pattern)>)> {
        let mut out = vec![];
        let tag = format!("{}", self.f.locals.len());
        let p = self.split_in(p, ty, &tag, &mut out);
        (!out.is_empty()).then_some((p, out))
    }

    fn split_in(
        &mut self,
        p: &ast::Pattern,
        ty: TyId,
        tag: &str,
        out: &mut Vec<(String, ast::Pattern)>,
    ) -> ast::Pattern {
        let ty = match self.cx.ty.opt_payload(ty) {
            Some(inner) if !matches!(p.kind, ast::PatternKind::Ident(_)) => inner,
            _ => ty,
        };
        let kind = match &p.kind {
            ast::PatternKind::Array { elems, rest } => {
                let elems = elems
                    .iter()
                    .enumerate()
                    .map(|(k, e)| match self.elem_type(ty, k) {
                        Some(t) => self.nested(e, t, tag, out),
                        None => e.clone(),
                    })
                    .collect();
                ast::PatternKind::Array {
                    elems,
                    rest: rest.clone(),
                }
            }
            ast::PatternKind::Object { fields, rest } => {
                let fields = fields
                    .iter()
                    .map(|(name, sub)| match self.cx.field_of(ty, &name.name) {
                        Some((_, t)) => (name.clone(), self.nested(sub, t, tag, out)),
                        None => (name.clone(), sub.clone()),
                    })
                    .collect();
                ast::PatternKind::Object {
                    fields,
                    rest: rest.clone(),
                }
            }
            _ => return p.clone(),
        };
        ast::Pattern {
            id: p.id,
            kind,
            span: p.span,
        }
    }

    /// A sub-pattern over a value of type `ty`: an array pattern over an iterable becomes a
    /// hidden name; others are split in turn.
    fn nested(
        &mut self,
        p: &ast::Pattern,
        ty: TyId,
        tag: &str,
        out: &mut Vec<(String, ast::Pattern)>,
    ) -> ast::Pattern {
        let ty = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        if matches!(p.kind, ast::PatternKind::Array { .. }) && self.is_consumable(ty) {
            let name = format!("<nested#{tag}.{}>", out.len());
            out.push((name.clone(), p.clone()));
            return ast::Pattern {
                id: ast::NodeId(u32::MAX),
                kind: ast::PatternKind::Ident(ast::Ident { name, span: p.span }),
                span: p.span,
            };
        }
        self.split_in(p, ty, tag, out)
    }

    /// The type of element `k` of an array or tuple type.
    fn elem_type(&self, ty: TyId, k: usize) -> Option<TyId> {
        match self.cx.ty.kind(ty) {
            TyKind::Array(e) => Some(*e),
            TyKind::Tuple(ts) => ts.get(k).copied(),
            _ => None,
        }
    }

    /// Declarations taking the split-off patterns apart (after the outer pattern bound them).
    pub(super) fn nested_decls(
        &mut self,
        kind: ast::VarKind,
        nested: Vec<(String, ast::Pattern)>,
        out: &mut Vec<crate::hir::Stmt>,
    ) {
        for (name, pattern) in nested {
            let span = pattern.span;
            let init = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Ident(ast::Ident { name, span }),
                span,
            };
            let decl = ast::VarDecl {
                kind,
                pattern,
                ty: None,
                init: Some(init),
                span,
            };
            self.var_decl(&decl, span, out);
        }
    }
}
