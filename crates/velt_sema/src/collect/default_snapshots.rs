//! `export default x;` of an imported `let` (#1916). `export default x` evaluates `x` where it
//! stands and exports that value, so a later change of the `let` doesn't show, as a re-export
//! (`export { x as default }`) would show it. The parser can't tell what an import binds, and
//! leaves the export list: what it binds is known here, once every module has declared its
//! items, and such a list gets the constant `default` the parser gives `export default expr;`.
//! A module's own declarations take precedence over its lists (`exports::build_table`), so the
//! constant is what the module exports.

use velt_common::Span;
use velt_syntax::ast;

use super::exports::{bound_name, imports_of, local_of};
use super::ItemDefs;
use crate::ctx::{Ctx, Item};

/// Declare the constant of each `export default x;` whose `x` is an imported `let`.
pub(super) fn declare(cx: &mut Ctx, items: &mut ItemDefs) {
    let modules = cx.modules.clone();
    for (m, module) in modules.iter().enumerate() {
        if module.is_std {
            continue;
        }
        for item in &module.ast.items {
            let Some((name, kw)) = default_of_ident(item) else {
                continue;
            };
            if !is_imported(cx, m, &name.name) {
                continue;
            }
            let Some(Item::Def(d)) = local_of(cx, m, &name.name) else {
                continue;
            };
            if cx.global(d).is_some_and(|g| g.mutable) {
                let constant = snapshot(name, kw, item.span);
                super::declare::declare_item(cx, m, constant, items);
                cx.export_memo.borrow_mut().changed();
            }
        }
    }
}

/// `export default x;` as the parser leaves it: `x`, and the `default` keyword's span.
fn default_of_ident(item: &ast::Item) -> Option<(&ast::Ident, Span)> {
    let ast::ItemKind::Import(imp) = &item.kind else {
        return None;
    };
    let [n] = imp.names.as_slice() else {
        return None;
    };
    let kw = n.alias.as_ref()?.span;
    (item.exported && velt_syntax::written_default(imp) && kw != item.span).then_some((&n.name, kw))
}

fn is_imported(cx: &Ctx, m: usize, name: &str) -> bool {
    imports_of(cx, m)
        .filter(|(imp, exported)| !exported && !imp.from.is_empty())
        .any(|(imp, _)| imp.names.iter().any(|n| bound_name(n).name == name))
}

/// The item `export const default = x;`, at `span`. Declarations borrow their syntax for the
/// whole check and the modules' syntax can't grow, so this one is kept for good: a few hundred
/// bytes per such export in one program.
fn snapshot(x: &ast::Ident, kw: Span, span: Span) -> &'static ast::Item {
    let synthetic = ast::NodeId(u32::MAX);
    let name = ast::Ident {
        name: super::exports::DEFAULT.to_string(),
        span: kw,
    };
    Box::leak(Box::new(ast::Item {
        kind: ast::ItemKind::Var(ast::VarDecl {
            kind: ast::VarKind::Const,
            pattern: ast::Pattern {
                id: synthetic,
                kind: ast::PatternKind::Ident(name),
                span: kw,
            },
            ty: None,
            init: Some(ast::Expr {
                id: synthetic,
                kind: ast::ExprKind::Ident(x.clone()),
                span,
            }),
            span,
        }),
        exported: true,
        span,
    }))
}

/// Are `name`'s interface and class declarations in module `m` both its default exports
/// (`export default interface I` and `export default class I`), where the parser marked the
/// interface's export as the type's? TypeScript merges them; Velt keeps the class (#384).
pub(super) fn default_type_and_class(cx: &Ctx, m: usize, name: &str) -> bool {
    imports_of(cx, m).any(|(imp, exported)| {
        exported
            && (imp.names.iter())
                .any(|n| velt_syntax::is_type_default(imp, n) && n.name.name == name)
    })
}
