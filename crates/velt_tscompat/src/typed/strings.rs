//! Rules on the shape of standard values: string positions, which are UTF-8 byte offsets in
//! Velt and UTF-16 code unit offsets in JavaScript (`string-offsets`), and `Map` iterators
//! used as arrays (`map-iter-as-array`): Velt's `keys()`, `values()` and `entries()` return
//! arrays, JavaScript's return iterators.

use velt_common::Span;
use velt_sema::ide::{LiteralKind, TypeView};
use velt_syntax::ast::{self, ExprKind as E, Lit};

use super::Typed;
use crate::{Fix, Severity};

/// The string members that count or take positions.
const OFFSET_MEMBERS: &[&str] = &[
    "length",
    "slice",
    "substring",
    "substr",
    "indexOf",
    "lastIndexOf",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "at",
    "padStart",
    "padEnd",
    "search",
];

/// `s.length`, `s.slice(…)`, … on a string, and `m.keys().length`.
pub(super) fn member(e: &ast::Expr, object: &ast::Expr, prop: &ast::Ident, t: &mut Typed) {
    iter_as_array(object, t);
    if OFFSET_MEMBERS.contains(&prop.name.as_str()) && is_string(object, t) {
        let what = format!("`{}`", prop.name);
        offsets(
            Span::new(e.span.file, object.span.lo, prop.span.hi),
            &what,
            t,
        );
    }
}

/// `s[i]` on a string, and `m.keys()[0]`.
pub(super) fn index(object: &ast::Expr, t: &mut Typed) {
    iter_as_array(object, t);
    if is_string(object, t) {
        offsets(object.span, "indexing", t);
    }
}

/// `e` is a string other than an ASCII string literal (whose offsets agree).
fn is_string(e: &ast::Expr, t: &Typed) -> bool {
    if let E::Lit(Lit::Str(s)) = &e.kind {
        return !s.is_ascii();
    }
    matches!(
        t.view_of(e),
        TypeView::Str | TypeView::Literal(LiteralKind::Str)
    )
}

fn offsets(span: Span, what: &str, t: &mut Typed) {
    t.cx.report(
        "string-offsets",
        Severity::Warning,
        span,
        format!("{what} on a string counts UTF-8 bytes in Velt, UTF-16 code units in JavaScript"),
        &[
            "lengths and positions agree for ASCII text only: `\"Zoë\".length` is 4 in Velt \
             and 3 in JavaScript",
            "keep shared code to ASCII text here, or work with whole strings (`split`, \
             `includes`, `startsWith`, `replace`)",
        ],
        None,
    );
}

/// `e` is `m.keys()`, `m.values()` or `m.entries()` on a `Map` (or a `Set`) and is used as an
/// array: Velt returns an array, JavaScript an iterator.
pub(super) fn iter_as_array(e: &ast::Expr, t: &mut Typed) {
    let E::Call { callee, args, .. } = &e.kind else {
        return;
    };
    let E::Member { object, prop, .. } = &callee.kind else {
        return;
    };
    if !args.is_empty() || !matches!(prop.name.as_str(), "keys" | "values" | "entries") {
        return;
    }
    let owner = match t.view_of(object) {
        TypeView::Map(..) => "Map",
        TypeView::Set(_) => "Set",
        _ => return,
    };
    let text = t.cx.text(e.span);
    let fix = Fix {
        span: e.span,
        replacement: format!("[...{text}]"),
        title: format!("copy into an array: `[...{text}]`"),
    };
    t.cx.report(
        "map-iter-as-array",
        Severity::Error,
        e.span,
        format!(
            "`{owner}.{}()` is an array in Velt, an iterator in TypeScript",
            prop.name
        ),
        &[
            "TypeScript's `keys()`, `values()` and `entries()` return iterators, which have no \
             `length`, indexing or array methods",
            "spread it into an array: `[...m.keys()]`, the same array in Velt",
        ],
        Some(fix),
    );
}
