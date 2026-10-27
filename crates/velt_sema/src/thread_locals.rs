//! A `ThreadLocal<T>` is named by the module constant holding it (velt_vir
//! `lower/thread_local.rs` gives each such constant its own thread-local storage). This pass,
//! once every constant's initializer is known, reports each `get` or `set` whose receiver is not
//! a module `const` initialized by `new ThreadLocal<T>(literal)`: a local, a parameter, a field,
//! a copy of the constant (`const b = a`) or a module `let` would each be another value.

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::ExprKind as E;

pub(crate) fn check(cx: &mut Ctx) {
    let uses = std::mem::take(&mut cx.thread_local_uses);
    let Some(tl) = cx.thread_local_ty() else {
        return;
    };
    for (global, span) in uses {
        let ok = global.is_some_and(|d| {
            cx.global(d).is_some_and(|g| {
                !g.mutable
                    && matches!(&g.init, Some(e) if matches!(&e.kind, E::AdtLit { def, .. } if *def == tl))
            })
        });
        if !ok {
            cx.error(
                Diagnostic::error(
                    "a `ThreadLocal` is read and written through the module constant that holds it",
                    span,
                )
                .with_note("declare it as `const name = new ThreadLocal<T>(…)` at module level, and call `name.get()` and `name.set(v)`"),
            );
        }
    }
}
