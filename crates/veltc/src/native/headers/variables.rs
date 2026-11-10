//! C variables (`declare c("lib") let name: T;`, or `const`) against their library's headers:
//! the headers must declare a variable of that name that a link symbol names (not `static`,
//! not thread-local), of the C type the declaration's type maps to, as a function's result
//! is compared. A `let`, which the program assigns, can't name a `const` variable.

use velt_common::{Diagnostic, Diagnostics};

use super::compare;
use super::ctype::Headers;
use super::Decl;

/// Check C variable `d` (assigned by the program when `writable`) against `h`, the parse of
/// `headers`, for a target whose `long` has `long_bits`.
pub(super) fn check(
    d: &Decl,
    writable: bool,
    h: &Headers,
    (headers, long_bits): (&[String], u32),
    diags: &mut Diagnostics,
) {
    let Some(c) = h.variables.get(&d.name) else {
        let mut diag = Diagnostic::error(
            format!(
                "C variable `{}` is not declared in its library's headers ({})",
                d.name,
                headers.join(", ")
            ),
            d.span,
        );
        if let Some(f) = h.functions.get(&d.name) {
            diag = diag.with_note(format!(
                "`{}` is a function in {}: declare it with `declare c(\"…\") function`",
                d.name, f.at
            ));
        }
        diags.push(diag);
        return;
    };
    if c.thread_local {
        diags.push(
            Diagnostic::error(
                format!(
                    "C variable `{}` is thread-local in {}: each thread has its own, which `declare c` can't name",
                    d.name, c.at
                ),
                d.span,
            )
            .with_note("use the library's function that gives the calling thread's (as `errno` is reached through `__errno_location()`)"),
        );
        return;
    }
    if c.internal {
        diags.push(
            Diagnostic::error(
                format!(
                    "C variable `{}` is `static` in {}: each file that includes the header has its own, under no link symbol",
                    d.name, c.at
                ),
                d.span,
            )
            .with_note("a program reaches only variables the library defines for others (`extern`)"),
        );
        return;
    }
    if writable && h.object_is_const(&c.ty) {
        diags.push(
            Diagnostic::error(
                format!(
                    "C variable `{}` is `{}` in {}, which C doesn't let a program assign",
                    d.name, c.ty, c.at
                ),
                d.span,
            )
            .with_note("declare it with `const`: `declare c(\"…\") const`"),
        );
        return;
    }
    if let Err(note) = compare::compare(&d.ret, &h.resolve(&c.ty, long_bits)) {
        diags.push(
            Diagnostic::error(
                format!(
                    "C variable `{}` is `{}` in {}, not the declaration's `{}`",
                    d.name,
                    c.ty,
                    c.at,
                    compare::describe(&d.ret)
                ),
                d.span,
            )
            .with_note(note),
        );
    }
}
