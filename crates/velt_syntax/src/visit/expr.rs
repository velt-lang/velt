//! Visiting expressions: an expression, the types written in it, then its sub-expressions
//! (arrow and function bodies and JSX included).

use crate::ast::{self, ExprKind as E};

use super::types::expr_types;
use super::{walk_block, walk_fn, Visit};

pub(super) fn opt_expr<'a>(e: Option<&'a ast::Expr>, v: &mut dyn Visit<'a>) {
    if let Some(e) = e {
        walk_expr(e, v);
    }
}

/// Visit `e`, the types written in it and its sub-expressions (arrow bodies and JSX included;
/// arrow parameter defaults not, as in [`super::walk_module`]).
pub fn walk_expr<'a>(e: &'a ast::Expr, v: &mut dyn Visit<'a>) {
    v.expr(e);
    expr_types(e, v);
    match &e.kind {
        E::Arrow { body, .. } => match body {
            ast::ArrowBody::Expr(x) => walk_expr(x, v),
            ast::ArrowBody::Block(b) => walk_block(b, v),
        },
        E::Function(f) => walk_fn(f, v),
        E::Object(props) | E::StructLit { props, .. } => {
            children(e, &mut |c| walk_expr(c, v));
            for p in props {
                if let ast::ObjectProp::Method(f) = p {
                    walk_fn(f, v);
                }
            }
        }
        _ => children(e, &mut |c| walk_expr(c, v)),
    }
}

/// Call `f` on every direct sub-expression of `e` (arrow, function and method bodies excluded).
fn children<'a>(e: &'a ast::Expr, f: &mut dyn FnMut(&'a ast::Expr)) {
    match &e.kind {
        E::Lit(_) | E::Ident(_) | E::This | E::Super | E::Arrow { .. } | E::Function(_) => {}
        E::Template { exprs, .. } | E::Array(exprs) => exprs.iter().for_each(f),
        E::Unary { expr, .. }
        | E::Update { target: expr, .. }
        | E::Spread(expr)
        | E::Await(expr)
        | E::Cast { expr, .. }
        | E::InstanceOf { expr, .. }
        | E::Paren(expr)
        | E::NonNull(expr)
        | E::Member { object: expr, .. } => f(expr),
        E::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        E::Assign { target, value, .. } => {
            f(target);
            f(value);
        }
        E::Cond { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::Call { callee, args, .. } => {
            f(callee);
            args.iter().for_each(f);
        }
        E::New { args, .. } => args.iter().for_each(f),
        E::Yield { arg: Some(a), .. } => f(a),
        E::Yield { arg: None, .. } => {}
        E::Index { object, index, .. } => {
            f(object);
            f(index);
        }
        E::Object(props) | E::StructLit { props, .. } => {
            for p in props {
                match p {
                    ast::ObjectProp::KeyValue(_, x) | ast::ObjectProp::Spread(x) => f(x),
                    ast::ObjectProp::Shorthand(_) | ast::ObjectProp::Method(_) => {}
                }
            }
        }
        E::Jsx(element) => jsx_exprs(element, f),
    }
}

/// Call `f` on every expression of a JSX element (attribute values, spreads, children), nested
/// elements included.
fn jsx_exprs<'a>(el: &'a ast::JsxElement, f: &mut dyn FnMut(&'a ast::Expr)) {
    use ast::{JsxAttr, JsxAttrValue, JsxChild};
    for attr in &el.attrs {
        match attr {
            JsxAttr::Spread { expr, .. }
            | JsxAttr::Named {
                value: Some(JsxAttrValue::Expr { expr, .. }),
                ..
            } => f(expr),
            JsxAttr::Named {
                value: Some(JsxAttrValue::Element(inner)),
                ..
            } => jsx_exprs(inner, f),
            JsxAttr::Named { .. } => {}
        }
    }
    for child in &el.children {
        match child {
            JsxChild::Expr {
                expr: Some(expr), ..
            }
            | JsxChild::Spread { expr, .. } => f(expr),
            JsxChild::Element(inner) => jsx_exprs(inner, f),
            JsxChild::Text { .. } | JsxChild::Expr { expr: None, .. } => {}
        }
    }
}
