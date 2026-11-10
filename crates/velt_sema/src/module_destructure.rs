//! Destructuring in module-level declarations (`export const { render } = renderer;`, sigx's
//! runtime-terminal): before collection, each such declaration becomes a hidden constant, the
//! result of the local declaration, and one declaration per name bound reading its field:
//!
//! ```ts ignore
//! export const { render, b: c = 1 } = renderer;
//! // const <destructured 120> = (() => {
//! //   const { render, b: c = 1 } = renderer;
//! //   return { render, c };
//! // })();
//! // export const render = <destructured 120>.render;
//! // export const c = <destructured 120>.c;
//! ```
//!
//! Local destructuring decides everything in JavaScript's order, once: getters on the way,
//! iteration, defaults, computed keys and rests.

use velt_common::Span;
use velt_syntax::ast;

use crate::SourceModule;

/// What a hidden constant's name starts with (`Ctx` names the declaration in its messages).
pub(crate) const HIDDEN: &str = "<destructured ";

/// The modules with their module-level destructuring rewritten, or `None` when none has any.
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    let has = |m: &SourceModule| m.ast.items.iter().any(|i| destructures(i).is_some());
    if !modules.iter().any(has) {
        return None;
    }
    let out = modules
        .iter()
        .map(|m| {
            let mut ast = m.ast.clone();
            if has(m) {
                let items = std::mem::take(&mut ast.items);
                ast.items = items.into_iter().flat_map(expand).collect();
            }
            SourceModule {
                path: m.path.clone(),
                is_std: m.is_std,
                is_dependency: m.is_dependency,
                file: m.file,
                ast,
                imports: m.imports.clone(),
                jsx_runtime: m.jsx_runtime.clone(),
            }
        })
        .collect();
    Some(out)
}

/// The declaration of `item` when it destructures.
fn destructures(item: &ast::Item) -> Option<&ast::VarDecl> {
    match &item.kind {
        ast::ItemKind::Var(v) if !matches!(v.pattern.kind, ast::PatternKind::Ident(_)) => Some(v),
        _ => None,
    }
}

/// `item`, or the declarations it stands for.
fn expand(item: ast::Item) -> Vec<ast::Item> {
    let Some(v) = destructures(&item) else {
        return vec![item];
    };
    let Some(init) = &v.init else {
        // A pattern needs a value; the parser or the collector reports it.
        return vec![item];
    };
    let span = v.span;
    let at = v.pattern.span;
    let hidden = ast::Ident {
        name: format!("{HIDDEN}{}>", at.lo),
        span: at,
    };
    let mut names = vec![];
    bound_names(&v.pattern, &mut names);
    let local = ast::VarDecl {
        kind: ast::VarKind::Const,
        pattern: v.pattern.clone(),
        ty: v.ty.clone(),
        init: Some(init.clone()),
        c_const: false,
        c_global: None,
        span,
    };
    let result = names
        .iter()
        .map(|n| ast::ObjectProp::Shorthand(n.clone()))
        .collect();
    let body = ast::Block {
        stmts: vec![
            ast::Stmt {
                kind: ast::StmtKind::Var(local),
                span,
            },
            ast::Stmt {
                kind: ast::StmtKind::Return(Some(expr(ast::ExprKind::Object(result), span))),
                span,
            },
        ],
        span,
    };
    let arrow = expr(
        ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Block(body),
            is_async: false,
        },
        span,
    );
    let call = expr(
        ast::ExprKind::Call {
            callee: Box::new(expr(ast::ExprKind::Paren(Box::new(arrow)), span)),
            type_args: vec![],
            args: vec![],
            optional: false,
        },
        span,
    );
    let mut out = vec![constant(&hidden, call, span, item.span)];
    for name in names {
        let read = expr(
            ast::ExprKind::Member {
                object: Box::new(ident(&hidden)),
                prop: name.clone(),
                optional: false,
            },
            name.span,
        );
        out.push(ast::Item {
            kind: ast::ItemKind::Var(ast::VarDecl {
                kind: v.kind,
                pattern: pattern(ast::PatternKind::Ident(name.clone()), name.span),
                ty: None,
                init: Some(read),
                c_const: false,
                c_global: None,
                span,
            }),
            exported: item.exported,
            span: item.span,
        });
    }
    out
}

/// `const name = init;`
fn constant(name: &ast::Ident, init: ast::Expr, span: Span, item: Span) -> ast::Item {
    ast::Item {
        kind: ast::ItemKind::Var(ast::VarDecl {
            kind: ast::VarKind::Const,
            pattern: pattern(ast::PatternKind::Ident(name.clone()), name.span),
            ty: None,
            init: Some(init),
            c_const: false,
            c_global: None,
            span,
        }),
        exported: false,
        span: item,
    }
}

/// The names `p` binds, in order.
fn bound_names(p: &ast::Pattern, out: &mut Vec<ast::Ident>) {
    match &p.kind {
        ast::PatternKind::Ident(id) => out.push(id.clone()),
        ast::PatternKind::Wildcard => {}
        ast::PatternKind::Default { pattern, .. } => bound_names(pattern, out),
        ast::PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, sub)| bound_names(sub, out));
            out.extend(rest.clone());
        }
        ast::PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|sub| bound_names(sub, out));
            out.extend(rest.clone());
        }
    }
}

fn pattern(kind: ast::PatternKind, span: Span) -> ast::Pattern {
    ast::Pattern {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn expr(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn ident(id: &ast::Ident) -> ast::Expr {
    expr(ast::ExprKind::Ident(id.clone()), id.span)
}
