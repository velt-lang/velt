//! Destructuring in module-level declarations (`export const { render } = renderer;`, sigx's
//! runtime-terminal): before collection, each such declaration becomes a hidden constant holding
//! the value and one declaration per name bound, each reading its part of that constant, as
//! the declarations a user would write by hand:
//!
//! ```ts ignore
//! export const { render, b: c } = renderer;
//! // const <destructured 120> = renderer;
//! // export const render = <destructured 120>.render;
//! // export const c = <destructured 120>.b;
//! ```
//!
//! An array pattern reads elements by index (`[a, , b]`) and a rest element is a `slice`.
//! Defaults (`{ a = 1 }`) and an object rest (`{ a, ...rest }`) stay errors: when a default
//! applies depends on the field's type (`undefined`, an absent optional field, but not a
//! declared `null`), which a local declaration decides while it is checked
//! (`body::pattern_defaults`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their module-level destructuring rewritten, or `None` when none has any;
/// with the errors found.
pub(crate) fn rewrite(modules: &[SourceModule]) -> (Option<Vec<SourceModule>>, Vec<Diagnostic>) {
    let has = |m: &SourceModule| m.ast.items.iter().any(|i| destructures(i).is_some());
    if !modules.iter().any(has) {
        return (None, vec![]);
    }
    let mut diags = vec![];
    let out = modules
        .iter()
        .map(|m| {
            let mut ast = m.ast.clone();
            if has(m) {
                let items = std::mem::take(&mut ast.items);
                ast.items = items
                    .into_iter()
                    .flat_map(|i| expand(i, &mut diags))
                    .collect();
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
    (Some(out), diags)
}

/// The declaration of `item` when it destructures.
fn destructures(item: &ast::Item) -> Option<&ast::VarDecl> {
    match &item.kind {
        ast::ItemKind::Var(v) if !matches!(v.pattern.kind, ast::PatternKind::Ident(_)) => Some(v),
        _ => None,
    }
}

/// `item`, or the declarations it stands for.
fn expand(item: ast::Item, diags: &mut Vec<Diagnostic>) -> Vec<ast::Item> {
    let Some(v) = destructures(&item) else {
        return vec![item];
    };
    let Some(init) = &v.init else {
        // A pattern needs a value; the parser or the collector reports it.
        return vec![item];
    };
    let span = v.span;
    let hidden = ast::Ident {
        name: format!("<destructured {}>", v.pattern.span.lo),
        span: v.pattern.span,
    };
    let mut out = vec![ast::Item {
        kind: ast::ItemKind::Var(ast::VarDecl {
            kind: ast::VarKind::Const,
            pattern: pattern(ast::PatternKind::Ident(hidden.clone()), v.pattern.span),
            ty: v.ty.clone(),
            init: Some(init.clone()),
            span,
        }),
        exported: false,
        span: item.span,
    }];
    let mut binds = vec![];
    parts(&v.pattern, ident(&hidden), &mut binds, diags);
    for (name, value) in binds {
        out.push(ast::Item {
            kind: ast::ItemKind::Var(ast::VarDecl {
                kind: v.kind,
                pattern: pattern(ast::PatternKind::Ident(name.clone()), name.span),
                ty: None,
                init: Some(value),
                span,
            }),
            exported: item.exported,
            span: item.span,
        });
    }
    out
}

/// The names `p` binds, each with the expression reading its part of `value`.
fn parts(
    p: &ast::Pattern,
    value: ast::Expr,
    out: &mut Vec<(ast::Ident, ast::Expr)>,
    diags: &mut Vec<Diagnostic>,
) {
    match &p.kind {
        ast::PatternKind::Ident(id) => out.push((id.clone(), value)),
        ast::PatternKind::Wildcard => {}
        ast::PatternKind::Default { pattern, .. } => {
            diags.push(Diagnostic::error(
                "a default (`= value`) in a module-level destructuring declaration is not supported yet",
                p.span,
            ).with_note("destructure inside a function, or read the part and apply the default: `const a = obj.a ?? 1;`"));
            parts(pattern, value, out, diags);
        }
        ast::PatternKind::Object { fields, rest } => {
            if let Some(r) = rest {
                diags.push(Diagnostic::error(
                    "a rest pattern (`...rest`) in a module-level declaration is not supported yet",
                    r.span,
                ));
            }
            for (key, sub) in fields {
                let read = expr(
                    ast::ExprKind::Member {
                        object: Box::new(value.clone()),
                        prop: key.clone(),
                        optional: false,
                    },
                    key.span,
                );
                parts(sub, read, out, diags);
            }
        }
        ast::PatternKind::Array { elems, rest } => {
            for (i, sub) in elems.iter().enumerate() {
                let at = int(i, sub.span);
                let read = expr(
                    ast::ExprKind::Index {
                        object: Box::new(value.clone()),
                        index: Box::new(at),
                        more: vec![],
                        optional: false,
                    },
                    sub.span,
                );
                parts(sub, read, out, diags);
            }
            if let Some(r) = rest {
                let from = int(elems.len(), r.span);
                out.push((r.clone(), method(&value, "slice", vec![from], r.span)));
            }
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

fn int(i: usize, span: Span) -> ast::Expr {
    expr(
        ast::ExprKind::Lit(ast::Lit::Int {
            value: i as u128,
            suffix: None,
        }),
        span,
    )
}

/// `object.name(args)`.
fn method(object: &ast::Expr, name: &str, args: Vec<ast::Expr>, span: Span) -> ast::Expr {
    let callee = expr(
        ast::ExprKind::Member {
            object: Box::new(object.clone()),
            prop: ast::Ident {
                name: name.to_string(),
                span,
            },
            optional: false,
        },
        span,
    );
    expr(
        ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args,
            optional: false,
        },
        span,
    )
}
