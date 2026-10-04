//! Semantic analysis: name resolution, type checking/inference, ownership analysis → [`hir::Program`].
//! `hir.rs` and the `check` signature are contracts (maintainer-owned).
//!
//! Pipeline inside `check` (after [`generic_arrows`] rewrites module-level generic arrows):
//! 1. [`collect`]: a definition per module-level item of every module (prelude modules'
//!    exports are visible everywhere), type shapes, signatures, vtables, interface impls.
//! 2. [`body`]: module constants, defaults, then every function body: resolve names, type-check
//!    bidirectionally (with type-argument inference) and desugar into HIR. Closures become
//!    their own function defs. [`instantiation_cycles`] then rejects generic recursion whose
//!    type arguments grow (it would have infinitely many instantiations).
//! 3. [`ownership`]: infer which params / receivers / bindings take ownership (fixpoint) and
//!    patch call sites; string moves become soft (strings are values); reject moves out of
//!    borrowed places.
//! 4. [`throws`]: infer each function's thrown type (fixpoint over the call graph); then
//!    [`record_keys`] (every `Record` key type, per instantiation), [`json`] (what JSON glue is
//!    generated for) and [`void_fields`] (no `void` fields).
//! 5. [`moves`]: flow-sensitive use-after-move / use-before-init analysis over the HIR; soft moves
//!    (async-call arguments, strings) used again become clones (`ownership::clone_reused`); then exclusive
//!    access per call (`ownership::check_exclusive`), thread boundaries, and what `Mutex.with`
//!    callbacks let past the lock (`ownership::check_locked`).
//! 6. `main` validation, [`finalize`] into a `hir::Program`.
//!
//! [`ide::check_for_ide`] runs steps 1–5 with side tables recorded for editors instead.
//!
//! Semantic decisions other stages rely on are documented on [`body`] and the modules above.

pub mod hir;

mod anon;
mod assigned_fields;
mod ast_walk;
mod body;
mod collect;
mod ctx;
mod defs;
mod discriminants;
mod dispatch;
mod finalize;
mod flow;
mod generic_arrows;
pub mod ide;
mod infer;
mod instantiation_cycles;
mod json;
mod known;
mod literals;
mod moves;
mod ownership;
mod promise_copies;
mod readonly;
mod record_keys;
mod resolve;
mod suggest;
mod throws;
mod ts_protocol;
mod type_defaults;
mod types;
mod unions;
mod utility_types;
mod visit;
mod void_fields;

use velt_common::{Diagnostic, Diagnostics, FileId, Span};
use velt_syntax::ast;

use crate::ctx::Item;
use crate::defs::{DefInfo, FnKind};

pub use dispatch::{instantiation_work, InstantiationWork};

/// One parsed module handed to sema by the driver.
pub struct SourceModule {
    /// Canonical module path: `"main"` for the root file, `"std/fs"`, `"./util"` resolved to a
    /// root-relative path like `"util"`, package modules as `"pkgname"` / `"pkgname/sub"`.
    pub path: String,
    /// Loaded from the standard library root (the loader's `Origin::Std`). Only std modules may
    /// use compiler intrinsics and the private members of std types; never derived from `path`,
    /// which user files must not be able to imitate.
    pub is_std: bool,
    pub file: FileId,
    pub ast: ast::Module,
    /// For each `import ... from "<spec>"` in this module: spec string → canonical path of the
    /// module it resolved to (the driver already loaded it into `modules`).
    pub imports: Vec<(String, String)>,
    /// Canonical path of the JSX runtime (`<jsxImportSource>/jsx-runtime`, see
    /// docs/internals/contracts/jsx.md) the driver loaded because this module contains JSX; `None` when it
    /// has none. Sema binds it as the namespace `JSX` and lowers JSX to calls of its exports.
    pub jsx_runtime: Option<String>,
}

/// Stack for the checking thread: the passes recurse over the AST/HIR (bounded by the parser's
/// nesting limit), which can exceed the 1 MB main-thread stack on Windows.
pub(crate) const SEMA_STACK_BYTES: usize = 64 << 20;

/// CONTRACT: check a whole program. `modules[root]` must define `main`.
/// Returns `Some(program)` iff there are no errors; warnings may accompany either outcome.
pub fn check(modules: &[SourceModule], root: usize) -> (Option<hir::Program>, Diagnostics) {
    check_with(modules, root, CheckOptions::default())
}

/// What [`check_with`] requires of the program beyond being well-typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckOptions {
    /// The root module must define `main` (a program to build or run). Without it a root with no
    /// `main` is a library module: every body is still checked and the program's `entry` is
    /// `None`; a `main` that is there is validated either way.
    pub require_main: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self { require_main: true }
    }
}

/// [`check`] with options (`velt check` checks library modules, which have no `main`).
pub fn check_with(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
) -> (Option<hir::Program>, Diagnostics) {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-sema".into())
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || check_on_current_thread(modules, root, opts));
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => check_on_current_thread(modules, root, opts),
        }
    })
}

fn check_on_current_thread(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
) -> (Option<hir::Program>, Diagnostics) {
    let lifted = generic_arrows::lift(modules);
    let modules = lifted.as_deref().unwrap_or(modules);
    let Some(root_mod) = modules.get(root) else {
        let d = Diagnostic::error("no root module to check", Span::DUMMY);
        return (None, vec![d]);
    };
    let mut cx = ctx::Ctx::new(modules, root);
    analyze(&mut cx);
    let entry = check_main(&mut cx, root, root_mod, opts.require_main);
    check_imported_scripts(&mut cx, root, modules);
    resolve::check_unused_aliases(&mut cx);

    if cx.diags.iter().any(|d| d.is_error()) {
        return (None, cx.diags);
    }
    finalize::build_defs(&mut cx);
    readonly::erase(&mut cx);
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
    // Growing generic recursion has infinitely many instantiations: the passes below propagate
    // requirements per instantiation and would never finish.
    if instantiation_cycles::check(cx) {
        return;
    }
    ownership::infer_modes(cx);
    body::expr::jsx::check_prop_copies(cx);
    throws::infer_all(cx);
    record_keys::check_instantiations(cx);
    json::check_json_types(cx);
    void_fields::check_instantiations(cx);
    ownership::soften_moves(cx);
    ownership::validate_moves(cx);
    let moved = moves::check_all(cx);
    ownership::clone_reused(cx, &moved.reused);
    ownership::box_cells(cx, &moved.boxed);
    ownership::check_exclusive(cx);
    ownership::check_boundaries(cx);
    ownership::check_locked(cx);
    ownership::check_many_threads(cx);
    promise_copies::check(cx);
}

/// Top-level statements run only in the root file: the parser turned an imported module's into
/// a `main` whose name has an empty span (`velt_syntax` `parser::script`).
fn check_imported_scripts(cx: &mut ctx::Ctx, root: usize, modules: &[SourceModule]) {
    for (i, m) in modules.iter().enumerate() {
        let script = m.ast.items.iter().find_map(|it| match &it.kind {
            velt_syntax::ast::ItemKind::Function(f)
                if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi =>
            {
                Some(f.sig.span)
            }
            _ => None,
        });
        if let (true, Some(span)) = (i != root, script) {
            cx.error(
                Diagnostic::error(
                    "top-level statements are only allowed in the file the program starts from",
                    span,
                )
                .with_note("an imported module runs nothing when it loads; put this code in a function and call it"),
            );
        }
    }
}

/// Validate the root module's `main`; a missing one is an error only when `require_main`.
fn check_main(
    cx: &mut ctx::Ctx,
    root: usize,
    root_mod: &SourceModule,
    require_main: bool,
) -> Option<hir::DefId> {
    let file_start = Span::new(root_mod.file, root_mod.ast.span.lo, root_mod.ast.span.lo);
    let main = cx.scopes[root].items.get("main").copied();
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
