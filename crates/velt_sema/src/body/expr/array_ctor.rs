//! JS array constructors: `new Array<T>(n).fill(v)` (n copies of `v`) and
//! `Array.from({ length: n }, (_, i) => f(i))`, rewritten to the prelude's `__arrayFilled` /
//! `__arrayFromLength` (one allocation each). A bare `new Array<T>(n)` would hold `n` holes
//! (`undefined`), which Velt has no value for, so it asks for `.fill(v)`; likewise the element
//! argument of `Array.from`'s callback is always `undefined` and must go unused.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// `new Array<T>(n).fill(v)` as `__arrayFilled<T>(n, v)`, if `object.prop(args)` is that.
    pub(super) fn array_fill_new(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let (type_args, n) = new_array(object)?;
        if prop.name != "fill" || args.len() != 1 {
            return None;
        }
        let call = synthetic_call(
            "__arrayFilled",
            type_args,
            vec![n.clone(), args[0].clone()],
            span,
        );
        Some(self.expr(&call, exp, Want::Move))
    }

    /// `new Array<T>(n)` not followed by `.fill(v)`: an error.
    pub(super) fn bare_new_array(&mut self, class: &ast::TypeExpr, span: Span) -> bool {
        let user_class = self.cx.lookup_item_at(self.module, "Array", span).is_some();
        if !is_array_name(class) || user_class {
            return false;
        }
        self.cx.error(
            velt_common::Diagnostic::error(
                "`new Array(n)` would hold `n` empty slots, which Velt has no value for",
                span,
            )
            .with_note(
                "write `new Array<T>(n).fill(value)` or `Array.from({ length: n }, (_, i) => ...)`",
            ),
        );
        true
    }

    /// `Array.from({ length: n }, (_, i) => e)` as `__arrayFromLength(n, (i) => e)`.
    pub(super) fn array_from(
        &mut self,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (Some(n), Some(f)) = (args.first().and_then(length_of), args.get(1)) else {
            self.cx.err(
                "`Array.from` supports `Array.from({ length: n }, (_, i) => ...)`",
                span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let Some(f) = index_only(f) else {
            self.cx.err(
                "the first argument of `Array.from`'s callback is always `undefined`: name it `_` (`(_, i) => ...`)",
                f.span,
            );
            return self.error_expr(span);
        };
        let call = synthetic_call("__arrayFromLength", vec![], vec![n.clone(), f], span);
        self.expr(&call, exp, Want::Move)
    }
}

/// `new Array<T>(n)`: (type args, n).
fn new_array(e: &ast::Expr) -> Option<(Vec<ast::TypeExpr>, &ast::Expr)> {
    match &e.kind {
        ast::ExprKind::New { class, args } if is_array_name(class) && args.len() == 1 => {
            match &class.kind {
                ast::TypeExprKind::Named { args: targs, .. } => Some((targs.clone(), &args[0])),
                _ => None,
            }
        }
        ast::ExprKind::Paren(inner) => new_array(inner),
        _ => None,
    }
}

fn is_array_name(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, .. } if path.len() == 1 && path[0].name == "Array")
}

/// `n` in `{ length: n }`.
fn length_of(e: &ast::Expr) -> Option<&ast::Expr> {
    match &e.kind {
        ast::ExprKind::Object(props) => match props.as_slice() {
            [ast::ObjectProp::KeyValue(k, v)] if k.name == "length" => Some(v),
            _ => None,
        },
        _ => None,
    }
}

/// `(_, i) => e` as `(i) => e` (the element parameter must be named `_…`).
fn index_only(f: &ast::Expr) -> Option<ast::Expr> {
    let ast::ExprKind::Arrow {
        type_params,
        params,
        ret,
        throws,
        body,
        is_async,
    } = &f.kind
    else {
        return None;
    };
    let [elem, index] = params.as_slice() else {
        return None;
    };
    if !elem.name.name.starts_with('_') {
        return None;
    }
    Some(ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Arrow {
            type_params: type_params.clone(),
            params: vec![index.clone()],
            ret: ret.clone(),
            throws: throws.clone(),
            body: body.clone(),
            is_async: *is_async,
        },
        span: f.span,
    })
}

fn synthetic_call(
    name: &str,
    type_args: Vec<ast::TypeExpr>,
    args: Vec<ast::Expr>,
    span: Span,
) -> ast::Expr {
    let callee = ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }),
        span,
    };
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args,
            args,
            optional: false,
        },
        span,
    }
}
