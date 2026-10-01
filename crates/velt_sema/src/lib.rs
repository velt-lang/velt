//! Semantic analysis: name resolution, type checking/inference, ownership analysis → [`hir::Program`].
//! `hir.rs` and the `check` signature are contracts (maintainer-owned).
//!
//! Pipeline inside `check`:
//! 1. [`collect`]: a definition per module-level item of every module (prelude modules'
//!    exports are visible everywhere), type shapes, signatures, vtables, interface impls.
//! 2. [`body`]: module constants, defaults, then every function body: resolve names, type-check
//!    bidirectionally (with type-argument inference) and desugar into HIR. Closures become
//!    their own function defs.
//! 3. [`ownership`]: infer which params / receivers / bindings take ownership (fixpoint) and
//!    patch call sites; string moves become soft (strings are values); reject moves out of
//!    borrowed places.
//! 4. [`throws`]: infer each function's thrown type (fixpoint over the call graph); then
//!    [`json`] (what JSON glue is generated for) and [`void_fields`] (no `void` fields).
//! 5. [`moves`]: flow-sensitive use-after-move / use-before-init analysis over the HIR; soft moves
//!    (async-call arguments, strings) used again become clones (`ownership::clone_reused`); then exclusive
//!    access per call (`ownership::check_exclusive`).
//! 6. `main` validation, [`finalize`] into a `hir::Program`.
//!
//! [`ide::check_for_ide`] runs steps 1–5 with side tables recorded for editors instead.
//!
//! Semantic decisions other stages rely on are documented on [`body`] and the modules above.

pub mod hir;

mod anon;
mod ast_walk;
mod body;
mod collect;
mod ctx;
mod defs;
mod discriminants;
mod finalize;
mod flow;
pub mod ide;
mod infer;
mod json;
mod known;
mod literals;
mod moves;
mod ownership;
mod resolve;
mod throws;
mod types;
mod unions;
mod visit;
mod void_fields;

use velt_common::{Diagnostic, Diagnostics, FileId, Span};
use velt_syntax::ast;

use crate::ctx::Item;
use crate::defs::{DefInfo, FnKind};

/// One parsed module handed to sema by the driver.
pub struct SourceModule {
    /// Canonical module path: `"main"` for the root file, `"std/fs"`, `"./util"` resolved to a
    /// root-relative path like `"util"`, package modules as `"pkgname"` / `"pkgname/sub"`.
    pub path: String,
    pub file: FileId,
    pub ast: ast::Module,
    /// For each `import ... from "<spec>"` in this module: spec string → canonical path of the
    /// module it resolved to (the driver already loaded it into `modules`).
    pub imports: Vec<(String, String)>,
}

/// Stack for the checking thread: the passes recurse over the AST/HIR (bounded by the parser's
/// nesting limit), which can exceed the 1 MB main-thread stack on Windows.
pub(crate) const SEMA_STACK_BYTES: usize = 64 << 20;

/// CONTRACT: check a whole program. `modules[root]` must define `main`.
/// Returns `Some(program)` iff there are no errors; warnings may accompany either outcome.
pub fn check(modules: &[SourceModule], root: usize) -> (Option<hir::Program>, Diagnostics) {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-sema".into())
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || check_on_current_thread(modules, root));
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => check_on_current_thread(modules, root),
        }
    })
}

fn check_on_current_thread(
    modules: &[SourceModule],
    root: usize,
) -> (Option<hir::Program>, Diagnostics) {
    let Some(root_mod) = modules.get(root) else {
        let d = Diagnostic::error("no root module to check", Span::DUMMY);
        return (None, vec![d]);
    };
    let mut cx = ctx::Ctx::new(modules, root);
    analyze(&mut cx);
    let entry = check_main(&mut cx, root, root_mod);

    if cx.diags.iter().any(|d| d.is_error()) {
        return (None, cx.diags);
    }
    finalize::build_defs(&mut cx);
    let ctx::Ctx {
        ty,
        defs,
        diags,
        impls,
        ..
    } = cx;
    let defs = defs
        .into_iter()
        .map(|d| d.expect("ICE: definition left unfilled without an error"))
        .collect();
    let program = hir::Program {
        types: ty.table,
        defs,
        entry,
        impls,
    };
    (Some(program), diags)
}

/// Steps 1–5: every definition and body checked, ownership and throws inferred, moves checked.
fn analyze(cx: &mut ctx::Ctx) {
    collect::collect(cx);
    body::check_bodies(cx);
    ownership::infer_modes(cx);
    throws::infer_all(cx);
    json::check_json_types(cx);
    void_fields::check_instantiations(cx);
    ownership::soften_string_moves(cx);
    ownership::validate_moves(cx);
    let reused = moves::check_all(cx);
    ownership::clone_reused(cx, &reused);
    ownership::check_exclusive(cx);
}

fn check_main(cx: &mut ctx::Ctx, root: usize, root_mod: &SourceModule) -> Option<hir::DefId> {
    let file_start = Span::new(root_mod.file, root_mod.ast.span.lo, root_mod.ast.span.lo);
    let Some(Item::Def(id)) = cx.scopes[root].items.get("main").copied() else {
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
