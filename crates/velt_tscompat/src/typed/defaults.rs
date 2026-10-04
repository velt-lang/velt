//! `null-default` (#431): a destructuring default applies when the property is `null` in Velt,
//! but only when it is `undefined` in JavaScript. A property whose type includes `null` (and
//! isn't optional, which JavaScript leaves `undefined`) gets the default in Velt and keeps the
//! `null` in JavaScript.

use velt_common::Span;
use velt_sema::ide::{DefKind, TypeView};
use velt_syntax::ast::{self, ExprKind as E, PatternKind as P};

use super::Typed;
use crate::{Fix, Severity};

/// A defaulted property of an object pattern whose value may be `null`.
struct Defaulted<'a> {
    key: &'a ast::Ident,
    /// The pattern the property is bound to (`{ x = 1 }`: `x`; `{ x: y = 1 }`: `y`).
    binding: &'a ast::Pattern,
    default: &'a ast::Expr,
    span: Span,
}

/// `const { x = d } = p` where `p.x` may be `null`.
pub(super) fn var_decl(v: &ast::VarDecl, t: &mut Typed) {
    let P::Object { fields, rest } = &v.pattern.kind else {
        return;
    };
    let Some(init) = &v.init else { return };
    let Some(object) = t.type_of(init) else {
        return;
    };
    let props = t.program.analysis.fields(&object);
    let mut found = vec![];
    for (key, pattern) in fields {
        let P::Default {
            pattern: binding,
            value,
        } = &pattern.kind
        else {
            continue;
        };
        let nullable = props
            .iter()
            .find(|f| f.name == key.name)
            .is_some_and(|f| matches!(t.view(&f.ty), TypeView::Nullable(_)));
        let optional = t
            .def(key.span)
            .is_some_and(|d| d.kind == DefKind::Field && t.decls.is_optional(d.span));
        if nullable && !optional {
            let span = Span::new(key.span.file, key.span.lo, pattern.span.hi.max(key.span.hi));
            found.push(Defaulted {
                key,
                binding,
                default: value,
                span,
            });
        }
    }
    let fixable = rest.is_none() && v.ty.is_none() && plain_path(init);
    // One fix rewrites the declaration for all of them; the first finding carries it.
    let mut fix = fixable
        .then(|| rewrite(v, init, fields, &found, t))
        .flatten();
    for d in &found {
        let message = format!(
            "the default of `{}` replaces `null` in Velt, but only `undefined` in JavaScript",
            d.key.name
        );
        t.cx.report(
            "null-default",
            Severity::Error,
            d.span,
            message,
            &[
                "the property's type includes `null`: Velt applies a destructuring default to \
                 `null` (it has no `undefined`), JavaScript keeps the `null`",
                "read the property with `??`: `const x = p.x ?? d` replaces `null` in both",
            ],
            fix.take(),
        );
    }
}

/// `p`, `this`, `p.q`: evaluating it twice reads the same value.
fn plain_path(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Ident(_) | E::This => true,
        E::Member {
            object,
            optional: false,
            ..
        } => plain_path(object),
        _ => false,
    }
}

/// `const { x = d, y } = p` → `const { y } = p;` and `const x = p.x ?? d` on the next line,
/// for every property in `found`.
fn rewrite(
    v: &ast::VarDecl,
    init: &ast::Expr,
    fields: &[(ast::Ident, ast::Pattern)],
    found: &[Defaulted],
    t: &Typed,
) -> Option<Fix> {
    let keyword = v.kind.keyword();
    let init_text = t.cx.text(init.span);
    let mut lines = vec![];
    let others: Vec<&str> = fields
        .iter()
        .filter(|(key, _)| !found.iter().any(|d| d.key.span == key.span))
        .map(|(key, p)| {
            t.cx.text(Span::new(
                key.span.file,
                key.span.lo,
                p.span.hi.max(key.span.hi),
            ))
        })
        .collect();
    if !others.is_empty() {
        lines.push(format!(
            "{keyword} {{ {} }} = {init_text}",
            others.join(", ")
        ));
    }
    for d in found {
        let P::Ident(name) = &d.binding.kind else {
            return None;
        };
        lines.push(format!(
            "{keyword} {} = {init_text}.{} ?? {}",
            name.name,
            d.key.name,
            t.cx.text(d.default.span)
        ));
    }
    let line_start = t.cx.src[..v.span.lo as usize]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let indent = &t.cx.src[line_start..v.span.lo as usize];
    if lines.len() > 1 && !indent.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    let names: Vec<String> = found.iter().map(|d| format!("`{}`", d.key.name)).collect();
    Some(Fix {
        span: Span::new(init.span.file, v.span.lo, init.span.hi),
        replacement: lines.join(&format!(";\n{indent}")),
        title: format!("read {} with `??`", names.join(", ")),
    })
}
