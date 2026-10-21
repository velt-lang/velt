//! The syntax the rewrite writes (see the parent module).

use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E, StmtKind as S};

const NO_ID: ast::NodeId = ast::NodeId(u32::MAX);

pub(super) fn expr(kind: E, span: Span) -> ast::Expr {
    ast::Expr {
        id: NO_ID,
        kind,
        span,
    }
}

pub(super) fn stmt(kind: S, span: Span) -> ast::Stmt {
    ast::Stmt { kind, span }
}

pub(super) fn ident(name: &str, span: Span) -> ast::Expr {
    expr(
        E::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }),
        span,
    )
}

pub(super) fn ident_pattern(name: &str, span: Span) -> ast::Pattern {
    ast::Pattern {
        id: NO_ID,
        kind: ast::PatternKind::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }),
        span,
    }
}

pub(super) fn named_type(name: &str, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: name.to_string(),
                span,
            }],
            args: vec![],
        },
        span,
    }
}

/// `I["k"]`.
pub(super) fn member_type(t: &ast::TypeExpr, k: &str) -> ast::TypeExpr {
    let span = t.span;
    let key = ast::TypeExpr {
        kind: ast::TypeExprKind::Literal(ast::SignedLit {
            lit: ast::Lit::Str(k.to_string()),
            negative: false,
        }),
        span,
    };
    ast::TypeExpr {
        kind: ast::TypeExprKind::Indexed {
            object: Box::new(t.clone()),
            key: Box::new(key),
        },
        span,
    }
}

/// `() => T`.
pub(super) fn getter_type(member: &ast::TypeExpr) -> ast::TypeExpr {
    fn_type(vec![], member.span, member.clone())
}

/// `(v: T) => void`.
pub(super) fn setter_type(member: &ast::TypeExpr) -> ast::TypeExpr {
    let unit = ast::TypeExpr {
        kind: ast::TypeExprKind::Void,
        span: member.span,
    };
    fn_type(vec![member.clone()], member.span, unit)
}

pub(super) fn fn_type(params: Vec<ast::TypeExpr>, span: Span, ret: ast::TypeExpr) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Function {
            params,
            ret: Box::new(ret),
            throws: None,
            rest: false,
        },
        span,
    }
}

/// `let <cell>: F | undefined = undefined;`: set right after the declaration, before any
/// accessor can run.
pub(super) fn let_cell(name: &str, f: ast::TypeExpr, span: Span) -> ast::Stmt {
    let undefined = ast::TypeExpr {
        kind: ast::TypeExprKind::Undefined,
        span,
    };
    let ty = ast::TypeExpr {
        kind: ast::TypeExprKind::Union(vec![f, undefined]),
        span,
    };
    stmt(
        S::Var(ast::VarDecl {
            kind: ast::VarKind::Let,
            pattern: ident_pattern(name, span),
            ty: Some(ty),
            init: Some(expr(E::Lit(ast::Lit::Undefined), span)),
            span,
        }),
        span,
    )
}

pub(super) fn assign(name: &str, value: ast::Expr, span: Span) -> ast::Stmt {
    let e = expr(
        E::Assign {
            op: None,
            target: Box::new(ident(name, span)),
            value: Box::new(value),
        },
        span,
    );
    stmt(S::Expr(e), span)
}

/// A setter function with its parameter typed `member` where the source left it out (the
/// interface typed it in the literal).
pub(super) fn typed_setter(mut f: ast::Expr, member: &ast::TypeExpr) -> ast::Expr {
    if let E::Arrow { params, .. } = &mut f.kind {
        for p in params.iter_mut().filter(|p| p.ty.is_none()) {
            p.ty = Some(member.clone());
        }
    }
    f
}

/// `() => <cell>!()`.
pub(super) fn get_forward(cell: &str, span: Span) -> ast::Expr {
    let call = expr(
        E::Call {
            callee: Box::new(expr(E::NonNull(Box::new(ident(cell, span))), span)),
            type_args: vec![],
            args: vec![],
            optional: false,
        },
        span,
    );
    arrow(vec![], call, span)
}

/// `(v: T) => <cell>!(v)`, named as setter `f` names its parameter.
pub(super) fn set_forward(cell: &str, f: &ast::Expr, member: &ast::TypeExpr) -> ast::Expr {
    let span = f.span;
    let name = match &f.kind {
        E::Arrow { params, .. } => params.first().map(|p| p.name.clone()),
        _ => None,
    }
    .unwrap_or(ast::Ident {
        name: "v".to_string(),
        span,
    });
    let param = ast::ArrowParam {
        name: name.clone(),
        pattern: None,
        ty: Some(member.clone()),
        default: None,
        optional: false,
        rest: false,
    };
    let call = expr(
        E::Call {
            callee: Box::new(expr(E::NonNull(Box::new(ident(cell, span))), span)),
            type_args: vec![],
            args: vec![expr(E::Ident(name), span)],
            optional: false,
        },
        span,
    );
    arrow(vec![param], call, span)
}

pub(super) fn arrow(params: Vec<ast::ArrowParam>, body: ast::Expr, span: Span) -> ast::Expr {
    expr(
        E::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(body)),
            is_async: false,
        },
        span,
    )
}
