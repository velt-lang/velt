//! Class fields without a type annotation (`count = 0;`, `done = false;`, TS infers the type):
//! the parser takes the type from the initializer when it states it plainly — a literal (`0` is
//! `i64`, `0.5` `f64`, `"a"` / templates `string`, `true` `bool`, a suffix names its type), `new
//! C<T>(…)`, or an array literal of such elements. Anything else needs `name: T = …`.

use super::Parser;
use crate::ast::*;

impl Parser<'_> {
    /// The type field initializer `e` states, or an error (`None`).
    pub(super) fn field_type_of(&mut self, name: &Ident, e: &Expr) -> Option<TypeExpr> {
        let ty = stated_type(e);
        if ty.is_none() {
            self.error(
                format!(
                    "cannot infer the type of field `{}` from its initializer; write `{}: T = ...`",
                    name.name, name.name
                ),
                e.span,
            );
        }
        ty
    }
}

/// The type initializer `e` plainly states (see the module docs).
fn stated_type(e: &Expr) -> Option<TypeExpr> {
    let named = |n: &str| TypeExpr {
        kind: TypeExprKind::Named {
            path: vec![Ident {
                name: n.to_string(),
                span: e.span,
            }],
            args: vec![],
        },
        span: e.span,
    };
    match &e.kind {
        ExprKind::Lit(Lit::Int { suffix, .. }) => Some(named(suffix.as_deref().unwrap_or("i64"))),
        ExprKind::Lit(Lit::Float { suffix, .. }) => Some(named(suffix.as_deref().unwrap_or("f64"))),
        ExprKind::Lit(Lit::Str(_)) | ExprKind::Template { .. } => Some(named("string")),
        ExprKind::Lit(Lit::Bool(_)) => Some(named("boolean")),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr,
        } => match &expr.kind {
            ExprKind::Lit(Lit::Int { .. } | Lit::Float { .. }) => stated_type(expr),
            _ => None,
        },
        ExprKind::New { class, .. } => Some(class.clone()),
        ExprKind::Array(elems) => {
            let first = stated_type(elems.first()?)?;
            let same = elems[1..]
                .iter()
                .all(|x| stated_type(x).is_some_and(|t| same_type(&t, &first)));
            same.then(|| TypeExpr {
                kind: TypeExprKind::Array(Box::new(first)),
                span: e.span,
            })
        }
        ExprKind::Paren(inner) => stated_type(inner),
        _ => None,
    }
}

/// Do two stated types name the same type (plain names and arrays of them)?
fn same_type(a: &TypeExpr, b: &TypeExpr) -> bool {
    match (&a.kind, &b.kind) {
        (
            TypeExprKind::Named { path: pa, args: aa },
            TypeExprKind::Named { path: pb, args: ab },
        ) => {
            aa.is_empty()
                && ab.is_empty()
                && pa.len() == pb.len()
                && pa.iter().zip(pb).all(|(x, y)| x.name == y.name)
        }
        (TypeExprKind::Array(x), TypeExprKind::Array(y)) => same_type(x, y),
        _ => false,
    }
}
