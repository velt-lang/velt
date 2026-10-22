//! Function types (`(a: A, ...xs: T[]) => R throws E`): the forms not supported yet. They are
//! resolved by `Ctx::resolve_fn_type` (`throws::fn_types`, which also gives a type written
//! without `throws` its inferred error type).

use velt_syntax::ast;

/// The function type forms Velt parses but doesn't support yet (#943): its error and a note.
pub(crate) fn unsupported_fn_type(kind: &ast::TypeExprKind) -> (&'static str, &'static str) {
    match kind {
        ast::TypeExprKind::Construct(_) => (
            "construct signatures (`new (...) => T`) are not supported yet",
            "a class can't be passed as a value yet; pass a function that constructs it (`(buf: ArrayBuffer) => new Uint8Array(buf)`)",
        ),
        _ => (
            "generic function types (`<T>(x: T) => R`) are not supported yet",
            "a function value has one type; declare a generic `function` and call it, or give the type parameters values (`(x: string) => R`)",
        ),
    }
}
