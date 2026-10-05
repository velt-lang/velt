//! Rules on the shape of standard values: `Map` iterators used as arrays
//! (`map-iter-as-array`): Velt's `keys()`, `values()` and `entries()` return arrays,
//! JavaScript's return iterators. (String lengths and positions count UTF-16 code units in both
//! since #377, so they need no rule.)

use velt_sema::ide::TypeView;
use velt_syntax::ast::{self, ExprKind as E};

use super::Typed;
use crate::{Fix, Severity};

/// `s.member` and `m.keys().length`.
pub(super) fn member(object: &ast::Expr, t: &mut Typed) {
    iter_as_array(object, t);
}

/// `m.keys()[0]`.
pub(super) fn index(object: &ast::Expr, t: &mut Typed) {
    iter_as_array(object, t);
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
