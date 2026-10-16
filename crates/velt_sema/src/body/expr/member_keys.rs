//! The constant key of a member access: `o.name`, `o["name"]`, `` o[`name`] ``.

use velt_syntax::ast;

/// `o.name` or `o["name"]` (a constant key, which reads the same field): the object and the
/// name. `None` for `?.` and other expressions.
pub(crate) fn member_view(e: &ast::Expr) -> Option<(&ast::Expr, ast::Ident)> {
    match &e.kind {
        ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } => Some((object, prop.clone())),
        ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } => Some((
            object,
            ast::Ident {
                name: literal_key(index)?,
                span: index.span,
            },
        )),
        _ => None,
    }
}

/// The key of `o["name"]` / `` o[`name`] ``: a string literal (a template without
/// substitutions).
pub(crate) fn literal_key(index: &ast::Expr) -> Option<String> {
    match &index.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.clone()),
        ast::ExprKind::Template { quasis, exprs } if exprs.is_empty() => quasis.first().cloned(),
        ast::ExprKind::Paren(inner) => literal_key(inner),
        _ => None,
    }
}
