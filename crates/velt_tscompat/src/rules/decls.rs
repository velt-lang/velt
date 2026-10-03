//! Rules on declarations TypeScript doesn't have (`struct`, `extend`, `interface-body`,
//! `throws`) or runs differently (`declare-fn`).

use velt_common::Span;
use velt_syntax::ast::{self, ItemKind as I};

use super::Cx;
use crate::Fix;

/// Check one item (its members and nested items are visited separately).
pub(super) fn item(item: &ast::Item, cx: &mut Cx) {
    match &item.kind {
        I::Struct(t) => cx.error(
            "struct",
            cx.keyword(item.span, "struct"),
            format!(
                "TypeScript has no `struct` declarations: `struct {}`",
                t.name.name
            ),
            &[
                "a Velt struct is a class whose values may be stored inline; TypeScript only \
                 has classes and object types",
                "write a `class`, or an object type (`type P = { … }`) built with object \
                 literals",
            ],
        ),
        I::Extend(_) => cx.error(
            "extend",
            cx.keyword(item.span, "extend"),
            "TypeScript has no `extend` blocks".into(),
            &[
                "Velt's `extend` adds methods to a type declared elsewhere; TypeScript can't add \
                 methods to a type it doesn't declare",
                "write a function that takes the value as its first parameter",
            ],
        ),
        I::Interface(i) => interface(i, cx),
        I::ExternFn(sig) => cx.error(
            "declare-fn",
            cx.keyword(item.span, "declare"),
            format!(
                "`declare function {}` binds native code, which JavaScript doesn't have",
                sig.name.name
            ),
            &[
                "TypeScript accepts the declaration and assumes JavaScript provides the \
                 function; calling it throws a `ReferenceError`",
                "keep native bindings out of code shared with TypeScript, and pass what they \
                 compute in",
            ],
        ),
        _ => {}
    }
}

/// Default method bodies, and `throws` on methods without one (methods with a body are
/// functions, checked by [`throws_clause`] as such).
fn interface(i: &ast::InterfaceDecl, cx: &mut Cx) {
    for m in &i.methods {
        if m.body.is_some() {
            cx.error(
                "interface-body",
                m.sig.name.span,
                format!(
                    "TypeScript interfaces can't have method bodies: `{}` has one",
                    m.sig.name.name
                ),
                &[
                    "a Velt interface may give a method a default body; a TypeScript interface \
                     only declares it",
                    "move the default into a base class, or into a function that takes the \
                     interface",
                ],
            );
        } else {
            throws_clause(&m.sig, false, cx);
        }
    }
}

/// `throws E` on a signature; `inferred` when Velt infers what the function throws without
/// it (a function with a body), so removing it is a fix.
pub(super) fn throws_clause(sig: &ast::FnSig, inferred: bool, cx: &mut Cx) {
    if let Some(t) = &sig.throws {
        throws(t, inferred, cx);
    }
}

/// `throws E` whose type is `t`, with a fix removing the clause when `fixable`.
pub(super) fn throws(t: &ast::TypeExpr, fixable: bool, cx: &mut Cx) {
    let before = cx.src.get(..t.span.lo as usize).unwrap_or("");
    let keyword = before.rfind("throws").unwrap_or(before.len());
    let clause = Span::new(t.span.file, keyword as u32, t.span.hi);
    let removed_from = before[..keyword].trim_end().len() as u32;
    let notes: &[&str] = if fixable {
        &[
            "Velt checks what a function throws; TypeScript doesn't track it",
            "remove the clause: Velt infers what the function throws",
        ]
    } else {
        &[
            "Velt checks what a function throws; TypeScript doesn't track it, and without a \
             body Velt can't infer it",
            "keep declarations that need `throws` out of code shared with TypeScript",
        ]
    };
    let message = format!("TypeScript has no `throws` clauses: `{}`", cx.text(clause));
    if fixable {
        let fix = Fix {
            span: Span::new(t.span.file, removed_from, t.span.hi),
            replacement: String::new(),
            title: "remove the `throws` clause".into(),
        };
        cx.error_with_fix("throws", clause, message, notes, fix);
    } else {
        cx.error("throws", clause, message, notes);
    }
}
