//! The root module's `main`: a program's entry point ([`check_main`]).

use velt_common::{Diagnostic, Span};

use crate::ctx::{self, Item};
use crate::defs::{DefInfo, FnKind};
use crate::{hir, SourceModule};

/// Validate the root module's `main`; a missing one is an error only when `require_main`.
pub(crate) fn check_main(
    cx: &mut ctx::Ctx,
    root: usize,
    root_mod: &SourceModule,
    require_main: bool,
) -> Option<hir::DefId> {
    let file_start = Span::new(root_mod.file, root_mod.ast.span.lo, root_mod.ast.span.lo);
    // A script that declares `main` itself starts at its statements (`velt_syntax::SCRIPT_MAIN`).
    let main = (cx.scopes[root].items.get(velt_syntax::SCRIPT_MAIN))
        .or_else(|| cx.scopes[root].items.get("main"))
        .copied();
    // A library module may use the name `main` for anything; only a `main` function is checked.
    let is_fn = |id: hir::DefId| matches!(cx.info[id.0 as usize], DefInfo::Fn(_));
    if !require_main && !matches!(main, Some(Item::Def(id)) if is_fn(id)) {
        return None;
    }
    let Some(Item::Def(id)) = main else {
        cx.error(Diagnostic::error(
            "`main` function not found in the root module",
            file_start,
        ));
        return None;
    };
    let f = match &cx.info[id.0 as usize] {
        DefInfo::Fn(f) => f,
        _ => {
            let span = cx.def_spans[id.0 as usize];
            cx.error(Diagnostic::error("`main` must be a function", span));
            return None;
        }
    };
    let span = f.name_span;
    // `async main` returns `Promise<void | i32>`; the runtime blocks on it.
    let ret = if f.is_async {
        cx.ty.async_result(f.ret)
    } else {
        f.ret
    };
    let (ret_span, kind, has_params, generic) = (
        f.ret_span,
        f.kind,
        !f.params.is_empty(),
        f.generics.len() > 0,
    );
    let mut ok = true;
    if kind == FnKind::Extern {
        cx.error(Diagnostic::error(
            "`main` must be a function with a body",
            span,
        ));
        ok = false;
    }
    if has_params || generic {
        cx.error(Diagnostic::error(
            "`main` function must not take parameters",
            span,
        ));
        ok = false;
    }
    if ret != cx.ty.unit && ret != cx.ty.i32 && ret != cx.ty.error {
        let found = cx.display(ret);
        cx.error(
            Diagnostic::error(
                "`main` must return `void` or `i32`",
                ret_span.unwrap_or(span),
            )
            .with_note(format!("found return type `{found}`")),
        );
        ok = false;
    }
    ok.then_some(id)
}
