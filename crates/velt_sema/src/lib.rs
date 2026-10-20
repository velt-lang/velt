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

mod absent_fields;
mod anon;
mod any;
mod assigned_fields;
mod ast_walk;
mod body;
mod brands;
mod call_sigs;
mod callables;
mod collect;
mod copy_flow;
mod creation_notes;
mod ctx;
mod defined_props;
mod defs;
mod discriminants;
mod dispatch;
pub mod effects;
mod finalize;
mod flow;
mod fresh_returns;
mod generic_arrows;
mod holds_promise;
mod hooks;
pub mod ide;
mod iface_writes;
mod index_signatures;
mod infer;
mod inst_sites;
mod instantiation_cycles;
mod interface_merge;
mod intersection_members;
mod intersections;
mod js_api;
mod json;
mod known;
mod literals;
mod module_chunks;
mod module_scripts;
mod module_state;
mod moves;
mod namespaces;
mod nested_closures;
mod object_copies;
mod overloads;
mod ownership;
mod promise_copies;
pub mod property_order;
mod readonly;
mod record_keys;
mod recursive_aliases;
mod recursive_unions;
mod replaced_holders;
mod resolve;
mod suggest;
mod symbols;
mod throws;
mod ts_protocol;
mod type_defaults;
mod type_level;
mod type_queries;
mod types;
mod undefined_args;
mod unions;
mod utility_types;
mod visit;
mod void_fields;
mod weak_threads;
mod written_types;

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

/// How much of the checking thread's stack inferring return types may use: the rest is left for
/// checking the deepest body (`body::returns::ret_of`).
pub(crate) const SEMA_STACK_BUDGET: usize = 48 << 20;

/// The budget when the checking thread could not be spawned and the caller's stack (of unknown
/// size, at least the 1 MB of a Windows main thread) is used instead.
pub(crate) const FALLBACK_STACK_BUDGET: usize = 512 << 10;

/// The address of a local of the caller's frame: how deep the stack is here (it grows down on
/// every supported target).
#[inline(never)]
pub(crate) fn stack_address() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

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
            .spawn_scoped(s, || {
                check_on_current_thread(modules, root, opts, &[], SEMA_STACK_BUDGET)
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => check_on_current_thread(modules, root, opts, &[], FALLBACK_STACK_BUDGET),
        }
    })
}

/// [`check_with`] of modules some of which the parser had to recover (`recovered`: their
/// indices in `modules` with their source text), for `velt check` to report the errors of the
/// other modules too. A name imported from a recovered module (directly or through re-exports)
/// that it doesn't declare but whose source has that word may be a declaration the parser
/// dropped: importing and using it reports nothing. The caller drops the diagnostics located in
/// recovered modules (their syntax errors come first). Never builds a program.
pub fn check_recovered(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    recovered: &[(usize, &str)],
) -> Diagnostics {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-sema".into())
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || {
                check_on_current_thread(modules, root, opts, recovered, SEMA_STACK_BUDGET).1
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => {
                check_on_current_thread(modules, root, opts, recovered, FALLBACK_STACK_BUDGET).1
            }
        }
    })
}

fn check_on_current_thread(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    recovered: &[(usize, &str)],
    stack_budget: usize,
) -> (Option<hir::Program>, Diagnostics) {
    let chunks = module_chunks::split(modules, root);
    let modules = chunks.modules.as_deref().unwrap_or(modules);
    let mut merge_diags = chunks.diags.clone();
    merge_diags.extend(call_sigs::check(modules));
    let (flat, ns_private, diags) = namespaces::flatten(modules);
    merge_diags.extend(diags);
    let modules = flat.as_deref().unwrap_or(modules);
    let (merged, diags) = interface_merge::rewrite(modules);
    merge_diags.extend(diags);
    let modules = merged.as_deref().unwrap_or(modules);
    let (callable, diags) = call_sigs::rewrite(modules);
    merge_diags.extend(diags);
    let modules = callable.as_deref().unwrap_or(modules);
    let lifted = generic_arrows::lift(modules);
    let modules = lifted.as_ref().map_or(modules, |l| &l.modules[..]);
    let overloaded = overloads::rewrite(modules);
    let modules = overloaded.as_ref().map_or(modules, |o| &o.modules[..]);
    let rewritten = recursive_aliases::rewrite(modules);
    let modules = rewritten.as_deref().unwrap_or(modules);
    let (brands, branded) = defined_props::rewrite(modules);
    let modules = branded.as_deref().unwrap_or(modules);
    let closures = nested_closures::rewrite(modules);
    let modules = closures.as_ref().map_or(modules, |c| &c.modules[..]);
    let Some(root_mod) = modules.get(root) else {
        let d = Diagnostic::error("no root module to check", Span::DUMMY);
        return (None, vec![d]);
    };
    let null_proto = modules
        .iter()
        .any(|m| !m.is_std && calls_object_create_null(&m.ast));
    let new_cx = |held_borrows: bool| {
        let mut cx = ctx::Ctx::new(modules, root);
        cx.stack_budget = stack_budget;
        cx.held_borrows = held_borrows;
        cx.ns_private = ns_private.clone();
        cx.gen_throw = ts_protocol::calls_throw(modules);
        if let Some(l) = &lifted {
            cx.generic_arrow_fns = l.local_fns.clone();
            cx.generic_arrow_all = l.all_fns.clone();
        }
        if let Some(o) = &overloaded {
            cx.has_overloads = true;
            cx.overloaded_methods = o.methods.clone();
        }
        cx.objects.null_proto_records = null_proto;
        cx.objects.brands = brands.clone();
        cx.recovered = (recovered.iter())
            .map(|&(m, src)| (m, collect::exports::words(src)))
            .collect();
        cx.diags.extend(merge_diags.iter().cloned());
        cx.promoted_consts = chunks.promoted.clone();
        if let Some(c) = &closures {
            cx.diags.extend(c.diags.iter().cloned());
            cx.kept_nested = c.kept.clone();
            cx.set_closure_groups(&c.groups);
        }
        let entry = analyze(&mut cx, root, root_mod, opts);
        if let Some(o) = &overloaded {
            overloads::report_misfits(&mut cx.diags, &o.sig_spans);
        }
        (cx, entry)
    };
    let (mut cx, mut entry) = new_cx(true);
    if retry_sharing(&cx) {
        (cx, entry) = new_cx(false);
    }
    if !recovered.is_empty() || cx.diags.iter().any(|d| d.is_error()) {
        return (None, cx.diags);
    }
    finalize::build_defs(&mut cx);
    readonly::erase(&mut cx);
    let module_state = module_state::build(&mut cx);
    let type_ops = type_level::lowered_ops(&mut cx);
    let anon_shapes = anon::concrete_shapes(&cx);
    let union_shapes = anon::concrete_unions(&cx);
    let dyn_members = any::dyn_members(&mut cx);
    let ctx::Ctx {
        ty,
        defs,
        diags,
        impls,
        counted_objects,
        objects,
        any,
        gen_throw,
        super_calls,
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
        // The HIR contract keeps std maps; the checker builds Fx maps.
        anon_shapes: anon_shapes.into_iter().collect(),
        union_shapes: union_shapes.into_iter().collect(),
        counted_objects,
        null_proto_records: objects.null_proto_records,
        module_state,
        type_ops: type_ops.into_iter().collect(),
        any_impl: any.and_then(|a| a.imp),
        unknown: any.and_then(|a| a.imp.map(|_| a.unknown)),
        object: any.and_then(|a| a.imp.map(|_| a.object)),
        function: any.and_then(|a| a.imp.map(|_| a.function)),
        dyn_members: dyn_members.map(|m| m.into_iter().collect()),
        generator_throw: gen_throw,
        super_calls: super_calls.into_iter().collect(),
    };
    (Some(program), diags)
}

/// Steps 1–5 and the checks of the whole program: the entry point, if there is one.
fn analyze(
    cx: &mut ctx::Ctx,
    root: usize,
    root_mod: &SourceModule,
    opts: CheckOptions,
) -> Option<hir::DefId> {
    analyze_bodies(cx);
    let entry = check_main(cx, root, root_mod, opts.require_main);
    resolve::check_unused_aliases(cx);
    entry
}

/// Should the program be checked again with held closures and `const me = this` sharing?
/// Borrowing in them only changes the cost, never which programs are accepted
/// (docs/reference/functions.md "Captures"). So when some of them borrowed and a pass from
/// `demote_local_closures` on reported an error, the program is checked again with them
/// sharing, as it was before. Errors from the earlier passes (type errors) don't depend on
/// borrowing and never cause a second check. The CLI and the IDE both use this rule.
fn retry_sharing(cx: &ctx::Ctx) -> bool {
    cx.held_borrows_used && cx.borrow_pass_errors
}

fn error_count(cx: &ctx::Ctx) -> usize {
    cx.diags.iter().filter(|d| d.is_error()).count()
}

/// Steps 1–5: every definition and body checked, ownership and throws inferred, moves checked.
fn analyze_bodies(cx: &mut ctx::Ctx) {
    collect::collect(cx);
    body::check_bodies(cx);
    module_scripts::declare(cx);
    absent_fields::check(cx);
    fresh_returns::check(cx);
    // Growing generic recursion has infinitely many instantiations: the passes below propagate
    // requirements per instantiation and would never finish.
    if instantiation_cycles::check(cx) {
        return;
    }
    // Before the passes that read instantiated types: deferred types reduced per instance.
    type_level::instances::reach(cx);
    hooks::check_shapes(cx);
    let before = error_count(cx);
    ownership_passes(cx);
    cx.borrow_pass_errors = error_count(cx) > before;
    // After `ownership_passes`: the JSON pass records the types `JSON.stringify` writes.
    object_copies::check(cx);
    replaced_holders::check(cx);
    cx.module_slots = module_state::order(cx);
    module_state::check_locked_access(cx);
}

/// The passes from `demote_local_closures` on: the ones whose errors may come from a held
/// closure or `const me = this` borrowing ([`retry_sharing`]).
fn ownership_passes(cx: &mut ctx::Ctx) {
    ownership::demote_local_closures(cx);
    ownership::infer_local_async(cx);
    ownership::infer_modes(cx);
    body::expr::jsx::check_prop_copies(cx);
    throws::infer_all(cx);
    hooks::check_throws(cx);
    record_keys::check_instantiations(cx);
    json::check_json_types(cx);
    void_fields::check_instantiations(cx);
    undefined_args::check_instantiations(cx);
    ownership::soften_moves(cx);
    ownership::validate_moves(cx);
    let moved = moves::check_all(cx);
    ownership::clone_reused(cx, &moved.reused);
    ownership::box_cells(cx, &moved.boxed, &moved.per_iteration);
    ownership::check_exclusive(cx);
    ownership::check_boundaries(cx);
    let weak = ownership::check_locked(cx);
    ownership::check_many_threads(cx);
    weak_threads::check(cx, weak);
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

/// Is `name` a property name a quoted key may not use: Velt's encodings of ES private names
/// (`#x`) and symbol keys (`[Symbol.iterator]`, and names starting with the noncharacter U+FFFF:
/// `property_order::SYMBOL_KEY_PREFIX`), or `__proto__`? Other names (`"[x]"`, `"[Symbol(k)]"`)
/// are ordinary keys: a computed key `[KEY]` is told apart by its span
/// (`symbols::is_computed_key`).
pub(crate) fn reserved_key(name: &str) -> bool {
    name.starts_with('#')
        || name.starts_with("[Symbol.")
        || name.starts_with(property_order::SYMBOL_KEY_PREFIX)
        || name == "__proto__"
}

/// Why property name `name` (a [`reserved_key`]) is not supported.
pub(crate) fn reserved_key_message(name: &str) -> String {
    if name == "__proto__" {
        return "the property name \"__proto__\" is not supported: in JavaScript it sets the object's prototype and creates no property".to_string();
    }
    format!(
        "the property name {name:?} is not supported: Velt uses names starting with {} for private names and symbol keys",
        reserved_prefixes(name)
    )
}

/// The reserved prefixes a message about property name `name` lists: U+FFFF too when `name`
/// starts with it (the key names of symbol constants).
pub(crate) fn reserved_prefixes(name: &str) -> &'static str {
    match name.starts_with(property_order::SYMBOL_KEY_PREFIX) {
        true => "`#`, `[Symbol.` and U+FFFF",
        false => "`#` and `[Symbol.`",
    }
}

/// Does `module` call `Object.create(null)`? Then records may have no prototype, which `in`,
/// printing and cloning test at run time (`hir::Program::null_proto_records`); other programs
/// get none of that code.
fn calls_object_create_null(module: &velt_syntax::ast::Module) -> bool {
    use velt_syntax::ast::{self, ExprKind as E};
    struct Find(bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            let E::Call { callee, args, .. } = &e.kind else {
                return;
            };
            let (E::Member { object, prop, .. }, [arg]) = (&callee.kind, args.as_slice()) else {
                return;
            };
            // As `object_create` accepts it: `null` in any parentheses.
            let mut arg = arg;
            while let E::Paren(inner) = &arg.kind {
                arg = inner;
            }
            if prop.name == "create"
                && matches!(&object.kind, E::Ident(o) if o.name == "Object")
                && matches!(arg.kind, E::Lit(ast::Lit::Null))
            {
                self.0 = true;
            }
        }
    }
    let mut f = Find(false);
    velt_syntax::visit::walk_module(module, &mut f);
    f.0
}
