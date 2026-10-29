//! Checking order: field defaults, module constants, parameter defaults, then every function
//! body. Bodies are checked on demand (`ensure_body`) because a `catch (e)` needs the thrown
//! types of the functions called in its `try`, which are only known once their bodies are.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{literal_locals, recheck, recursion, FnCx, Frame, LocalKind, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, FnSource, RetSource};
use crate::hir::{self, Def, DefId, ExprKind as H, TyId, TyKind};
use crate::resolve::TyEnv;

/// How many times a body is checked again for the types of its locals declared from literals
/// (`literal_locals`): one decision may let another local be decided.
const LITERAL_PASSES: usize = 3;

pub(crate) fn check_bodies(cx: &mut Ctx) {
    super::literal_hosts::find(cx);
    let all: Vec<DefId> = (0..cx.info.len() as u32).map(DefId).collect();
    for &d in &all {
        field_defaults(cx, d);
    }
    for &d in &all {
        if cx.global(d).is_some() {
            ensure_global(cx, d);
        }
    }
    let fns = cx.fn_defs.clone();
    for &d in &fns {
        super::defaults::param_defaults(cx, d);
    }
    super::defaults::iface_defaults(cx);
    super::literal_hosts::ensure_all(cx);
    for d in fns {
        ensure_body(cx, d);
    }
    super::returns::check_deferred(cx);
    static_copy_uses(cx);
    super::expr::iface_from_object::check_unwraps(cx);
}

/// The errors of copies of static methods (`Ctx::copy_diags`) whose copy the program uses: one
/// named by a function that is no copy, or by a used copy. Each is reported once.
fn static_copy_uses(cx: &mut Ctx) {
    if cx.copy_diags.is_empty() {
        return;
    }
    let mut used: rustc_hash::FxHashSet<DefId> = rustc_hash::FxHashSet::default();
    loop {
        let before = used.len();
        for &(from, copy) in &cx.copy_uses {
            let live =
                from.is_none_or(|f| !cx.static_copy_of.contains_key(&f) || used.contains(&f));
            if live {
                used.insert(copy);
            }
        }
        if used.len() == before {
            break;
        }
    }
    let mut seen = rustc_hash::FxHashSet::default();
    for (copy, d) in std::mem::take(&mut cx.copy_diags) {
        let key = (
            format!("{:?}", d.labels.first().map(|l| l.span)),
            d.message.clone(),
        );
        if used.contains(&copy) && seen.insert(key) {
            cx.error(d);
        }
    }
}

/// A checker for an expression outside any function body (defaults, constants).
pub(super) fn detached<'a, 'm>(
    cx: &'a mut Ctx<'m>,
    module: usize,
    generics: &[String],
) -> FnCx<'a, 'm> {
    let env = TyEnv::new(module, generics);
    let mut fcx = FnCx::new(cx, module, env, Frame::new(FnKind::Free, None));
    fcx.detached = true;
    fcx
}

/// Can values of type `t` be printed / formatted (`FnCx::printable`), checked outside a body?
pub(crate) fn printable(cx: &mut Ctx, t: TyId) -> bool {
    detached(cx, 0, &[]).printable(t)
}

/// Check the own field defaults of type `d` (once), recording what each may throw.
pub(crate) fn field_defaults(cx: &mut Ctx, d: DefId) {
    if cx.adt(d).is_none_or(|a| a.decl.is_none()) || !cx.field_defaults_checked.insert(d) {
        return;
    }
    let a = cx.adt(d).expect("ICE: adt");
    let (decl, module, names, start, qual) = (
        a.decl.expect("ICE: decl"),
        a.module,
        a.generics.names.clone(),
        a.own_fields_start,
        a.qual_name.clone(),
    );
    let fields = cx.adt(d).expect("ICE: adt").fields[start..].to_vec();
    let saved = std::mem::replace(&mut cx.display_params, names.clone());
    for (i, f) in fields.iter().enumerate() {
        let Some(ast_field) = decl.fields.iter().find(|af| {
            af.name.name == f.name || cx.resolve_key(module, &af.name).ok() == Some(f.name.clone())
        }) else {
            continue;
        };
        let mut throws = vec![];
        let default = match &ast_field.default {
            Some(e) => {
                let mut fcx = detached(cx, module, &names);
                fcx.owner = Some(d);
                fcx.fn_name = format!("{qual}.{}", f.name);
                let h = fcx.expr_coerce(e, f.ty, Want::Move);
                throws = std::mem::take(&mut fcx.f.uncaught);
                Some(h)
            }
            None if f.optional && cx.dynamic(f.ty).is_some() => {
                // `q?: any` left out holds the dynamic `undefined`, as in JavaScript.
                let mut fcx = detached(cx, module, &names);
                Some(fcx.dynamic_nothing(hir::Nothing::Undefined, f.ty, f.span))
            }
            None if f.optional => Some(hir::Expr {
                kind: H::Lit(hir::Lit::Null),
                ty: f.ty,
                span: f.span,
            }),
            None => None,
        };
        let field = &mut cx.adt_mut(d).fields[start + i];
        field.default = default;
        field.default_throws = throws;
    }
    // An inherited field given an initializer here (an error class's `name = "AppError"`).
    let redefined = cx.adt(d).expect("ICE: adt").redefined.clone();
    for (k, r) in redefined.iter().enumerate() {
        let Some(e) = decl
            .fields
            .iter()
            .find(|af| af.name.name == r.name)
            .and_then(|af| af.default.as_ref())
        else {
            continue;
        };
        let fty = cx.adt(d).expect("ICE: adt").fields[r.field].ty;
        let mut fcx = detached(cx, module, &names);
        fcx.owner = Some(d);
        fcx.fn_name = format!("{qual}.{}", r.name);
        let h = fcx.expr_coerce(e, fty, Want::Move);
        let throws = std::mem::take(&mut fcx.f.uncaught);
        let r = &mut cx.adt_mut(d).redefined[k];
        r.init = Some(h);
        r.throws = throws;
    }
    cx.display_params = saved;
}

/// Check a module-level constant (on first use or in declaration order).
pub(crate) fn ensure_global(cx: &mut Ctx, d: DefId) {
    let Some(g) = cx.global(d) else { return };
    match g.state {
        BodyState::Done => return,
        BodyState::InProgress => {
            // Read in a function value of its own initializer (`const r: R = { m() { return r; } }`):
            // the function runs once the constant is initialized, as in Node, so the constant is
            // kept in the module state (a read before its initialization throws Node's
            // `ReferenceError`), of its written or inferred type.
            if cx.fn_value_depth > 0 && (g.ty != cx.ty.error || cx.typing_globals.contains(&d)) {
                if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
                    g.stored = true;
                }
                return;
            }
            let span = g.span;
            if cx.fn_value_depth > 0 && cx.checking.is_empty() {
                let name = g.name.clone();
                cx.error(
                    Diagnostic::error(
                        format!("type annotation needed for `{name}`: a closure in its initializer uses it"),
                        span,
                    )
                    .with_note(format!("write its type, e.g. `const {name}: T = …`")),
                );
                cx.global_cycles += 1;
                return;
            }
            // The function bodies being checked lead from the initializer back to the constant.
            let chain: Vec<String> = cx
                .checking
                .iter()
                .map(|&f| {
                    let n = &cx.fn_info(f).name;
                    format!("`{}`", n.rsplit("::").next().unwrap_or(n))
                })
                .collect();
            let msg = match chain.is_empty() {
                false => format!(
                    "module constant `{}` depends on itself through {}",
                    g.name,
                    chain.join(" -> ")
                ),
                true => "module-level constant refers to itself".to_string(),
            };
            cx.err(msg, span);
            cx.global_cycles += 1;
            return;
        }
        BodyState::Unchecked => {}
    }
    let (module, qual, owner, mutable) = (g.module, g.qual_name.clone(), g.src.owner, g.mutable);
    // A `const` kept in module state from the start (`module_chunks`), like a `let`.
    let kept = g.stored;
    let (ann, init_expr, decl_span) = (g.src.ann, g.src.init, g.src.span);
    set_global_state(cx, d, BodyState::InProgress);
    cx.literal_hosts.in_constants += 1;
    let depth = std::mem::take(&mut cx.fn_value_depth);
    let saved = std::mem::take(&mut cx.display_params);
    let mut fcx = detached(cx, module, &[]);
    fcx.fn_name = qual;
    fcx.owner = owner;
    // Where its type is written, for the sites in it (`throws::promise_sites`).
    let written = ann.or(init_expr.and_then(crate::throws::written_init_type));
    if let (Some(w), DefInfo::Global(g)) = (written, &fcx.cx.info[d.0 as usize]) {
        let at = g.span;
        fcx.cx.note_written_type(at, w.span);
    }
    let ann = ann.map(|t| fcx.cx.resolve_type(t, &TyEnv::new(module, &[])));
    if let (Some(t), DefInfo::Global(g)) = (ann, &mut fcx.cx.info[d.0 as usize]) {
        g.ty = t;
    }
    let own_symbol = fcx.cx.symbol_init(d, ann);
    let init = match (init_expr, ann) {
        _ if own_symbol.is_some() => own_symbol,
        (Some(e), Some(t)) => Some(fcx.expr_coerce(e, t, Want::Move)),
        (Some(e), None) => {
            self_typed(&mut fcx, d, e);
            Some(fcx.expr(e, None, Want::Move))
        }
        // `let x: T | undefined;` starts as `undefined`, as in JavaScript.
        (None, Some(t))
            if mutable
                && owner.is_none()
                && (fcx.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined())
                    || fcx.cx.dynamic(t).is_some()) =>
        {
            let u = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Lit(ast::Lit::Undefined),
                span: decl_span,
            };
            Some(fcx.expr_coerce(&u, t, Want::Move))
        }
        (None, _) => {
            let what = if owner.is_some() {
                "static fields must be initialized"
            } else if mutable {
                "a module-level `let` must be initialized"
            } else {
                "`const` declarations must be initialized"
            };
            fcx.cx.err(what, decl_span);
            None
        }
    };
    cx.fn_value_depth = depth;
    let ty = ann.or(init.as_ref().map(|i| i.ty)).unwrap_or(cx.ty.error);
    // Kept as well when a function value in the initializer reads the constant.
    let mut stored = mutable || kept || cx.global(d).is_some_and(|g| g.stored);
    // A top-level constant that reads a module `let` (or a property of `globalThis`) holds
    // the value it had when Node evaluated the constant, once: evaluated at each use, it would
    // see later writes. Kept in the module state, it is evaluated in Node's order instead.
    if let (Some(i), false) = (&init, stored) {
        let top_level = !cx.modules[module].is_std;
        if top_level && ty != cx.ty.error && crate::module_state::reads_state(cx, i) {
            stored = true;
        }
    }
    // A constant holding an `Atomic` is one word the program changes in place (atomics.md):
    // evaluated at each use, every use would change a new one. `new Atomic<T>(literal)` is
    // that word in the program's data instead, with no module state (a static atomic).
    let static_atomic = !mutable
        && owner.is_none()
        && init
            .as_ref()
            .is_some_and(|i| super::expr::static_atomic_init(cx, i));
    if let (Some(_), false) = (&init, stored || static_atomic) {
        if !cx.modules[module].is_std && ty != cx.ty.error && cx.holds_atomic(ty) {
            stored = true;
        }
    }
    if let (Some(i), false) = (&init, stored) {
        let name = cx.global(d).map(|g| g.name.clone()).unwrap_or_default();
        let cycles = cx.global_cycles;
        let mut own = Vec::new();
        super::pure_init::check_init(cx, &name, i, ty, &mut own);
        // A module `const` that is not a constant is evaluated once per process, as Node evaluates
        // it once (#802): kept in the module state instead of being an error. So is a function
        // from a call, whose identity shows. Only `check_init`'s own diagnostics go then: those
        // of the functions it calls (checked on the way) stay.
        let top_level = !cx.modules[module].is_std;
        let function = super::pure_init::has_call(i)
            && matches!(
                cx.ty.kind(ty),
                crate::hir::TyKind::FnPtr { .. } | crate::hir::TyKind::Closure(_)
            );
        // A constant that depends on itself stays an error (its value is never known).
        // So does one that is async: module state is initialized where nothing can await. One
        // that may throw is kept: an error it throws ends the process while the module state
        // is initialized, as Node stops at an error thrown while it evaluates a module.
        let cycle = cx.global_cycles != cycles || own.iter().any(|o| o.fatal);
        if top_level && (!own.is_empty() || function) && !cycle && ty != cx.ty.error {
            stored = true;
        } else {
            // A top-level constant whose type is already an error (reported where it arose)
            // would be module state otherwise: only an async initializer stays worth reporting.
            let errored = top_level && ty == cx.ty.error;
            let own = own.into_iter().filter(|o| o.fatal || !errored);
            cx.diags.extend(own.map(|o| o.diag));
        }
    }
    if stored && ty != cx.ty.error {
        promise_in_state(cx, d, ty, decl_span);
    }
    cx.display_params = saved;
    cx.literal_hosts.in_constants -= 1;
    if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
        g.ty = ty;
        g.init = init;
        g.stored = stored;
        g.state = BodyState::Done;
    }
    cx.rec.globals_done.push(d);
}

/// Module state cannot hold a promise itself: it is initialized before `main` runs, outside
/// async code, where a promise does not start until something awaits it (in Node it starts at
/// once). A class, an array or a `Map` in module state may hold promises that tasks put there:
/// each read is another holder of the shared promise (#212), and the tasks that use module
/// state run one at a time.
pub(crate) fn promise_in_state(
    cx: &mut Ctx,
    d: DefId,
    ty: crate::hir::TyId,
    span: velt_common::Span,
) {
    if !holds_promise_by_value(cx, ty, 0) {
        return;
    }
    let name = cx.global(d).map(|g| g.name.clone()).unwrap_or_default();
    let shown = cx.display(ty);
    cx.error(
        velt_common::Diagnostic::error(
            format!("module-level `{name}` holds a promise (`{shown}`): module state cannot hold one"),
            span,
        )
        .with_note("module state is initialized before `main` runs, outside async code, where a promise would not start until something awaits it")
        .with_note("start the work in `main` (`const p = work();`) and pass the promise, or its result, where it is needed"),
    );
}

/// Does a value of `t` hold a promise itself (not behind a class, an array or a `Map`, which
/// tasks fill as they run)?
pub(crate) fn holds_promise_by_value(cx: &Ctx, t: crate::hir::TyId, depth: u32) -> bool {
    use crate::hir::{AdtKind, TyKind};
    if depth > 8 {
        return false;
    }
    match cx.ty.kind(t) {
        TyKind::Promise(..) => true,
        TyKind::Option(x, _) => holds_promise_by_value(cx, *x, depth + 1),
        TyKind::Tuple(xs) => xs.iter().any(|x| holds_promise_by_value(cx, *x, depth + 1)),
        // An object literal type or a struct holds its fields inline.
        TyKind::Adt(d, args) => match cx.adt(*d) {
            Some(a) if matches!(a.kind, AdtKind::Anon | AdtKind::Struct) => {
                a.fields.iter().any(|f| {
                    let t = match cx.ty.kind(f.ty) {
                        TyKind::Param(i) => args.get(*i as usize).copied().unwrap_or(f.ty),
                        _ => f.ty,
                    };
                    holds_promise_by_value(cx, t, depth + 1)
                })
            }
            _ => false,
        },
        _ => false,
    }
}

/// Module constant `d` without a written type, whose initializer `e` reads it inside a function
/// value (`const Level = component<P>(() => jsx(Level, …))`): typed by a trial check of `e` in
/// which those reads are of unknown type, when that gives `e` a type without them (TypeScript
/// infers it the same way). The reads then have that type.
fn self_typed(fcx: &mut FnCx, d: DefId, e: &velt_syntax::ast::Expr) {
    let Some(name) = fcx.cx.global(d).map(|g| g.name.clone()) else {
        return;
    };
    if !reads_in_fn_value(e, &name) {
        return;
    }
    let kept = fcx.cx.global(d).is_some_and(|g| g.stored);
    let defs = fcx.cx.info.len() as u32;
    fcx.cx.typing_globals.push(d);
    let t = fcx.trial_type(e);
    fcx.cx.typing_globals.pop();
    // Not a type the trial made (an object literal's struct or interface): it was rolled back.
    let t = t.filter(|&t| {
        let mut made = false;
        fcx.cx.ty.visit(t, &mut |k| {
            made |= matches!(k, TyKind::Adt(x, _) | TyKind::Dyn(x, _) | TyKind::Closure(x) if x.0 >= defs);
        });
        !made && !fcx.cx.ty.has_error(t)
    });
    if let DefInfo::Global(g) = &mut fcx.cx.info[d.0 as usize] {
        g.stored = kept;
        if let Some(t) = t {
            g.ty = t;
        }
    }
}

/// Does `e` name `name` inside an arrow or function expression?
fn reads_in_fn_value(e: &velt_syntax::ast::Expr, name: &str) -> bool {
    use velt_syntax::ast::ExprKind as E;
    use velt_syntax::visit::{walk_expr, Visit};
    struct Names<'n>(&'n str, bool);
    impl Visit<'_> for Names<'_> {
        fn expr(&mut self, e: &velt_syntax::ast::Expr) {
            self.1 |= matches!(&e.kind, E::Ident(i) if i.name == self.0);
        }
    }
    struct Fns<'n>(&'n str, bool);
    impl Visit<'_> for Fns<'_> {
        fn expr(&mut self, e: &velt_syntax::ast::Expr) {
            if !self.1 && matches!(e.kind, E::Arrow { .. } | E::Function(_)) {
                let mut n = Names(self.0, false);
                walk_expr(e, &mut n);
                self.1 = n.1;
            }
        }
    }
    let mut f = Fns(name, false);
    walk_expr(e, &mut f);
    f.1
}

fn set_global_state(cx: &mut Ctx, d: DefId, s: BodyState) {
    if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
        g.state = s;
    }
}

/// Check `def`'s body now unless it is already checked or being checked. A body whose
/// inferred return type was needed before it was known is checked again (`body::recursion`).
pub(crate) fn ensure_body(cx: &mut Ctx, def: DefId) {
    let f = cx.fn_info(def);
    if f.state != BodyState::Unchecked {
        return;
    }
    let Some(src) = f.source else {
        cx.fn_info_mut(def).state = BodyState::Done;
        return;
    };
    let inferred = f.ret_source == RetSource::Body;
    // A copy of a static method (`collect::static_this`) reports only what the method's own
    // check doesn't.
    let copy_of = cx.static_copy_of.get(&def).copied();
    if let Some(m) = copy_of {
        ensure_body(cx, m);
    }
    let diags_before = cx.diags.len();
    cx.fn_info_mut(def).state = BodyState::InProgress;
    let mark = recheck::Mark::new(cx, def);
    recheck::enter(cx);
    let mut fndef = check_body(cx, def, src);
    let recursive = recursion::needs_second_pass(cx, def, &mark);
    if recursive {
        fndef = check_body(cx, def, src);
    }
    // Locals declared from literals that the body uses as one integer type: checked again
    // with that type (`literal_locals`); each check may decide more of them.
    for _ in 0..LITERAL_PASSES {
        if !literal_locals::decide(cx, def) {
            break;
        }
        mark.rollback_own(cx);
        if inferred && !recursive {
            cx.fn_info_mut(def).ret_source = RetSource::Body;
        }
        fndef = check_body(cx, def, src);
    }
    literal_locals::finish(cx, def, mark.lens().diags());
    if copy_of.is_some() {
        drop_repeated_diags(cx, def, diags_before);
    }
    recheck::leave(cx, mark.lens());
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    cx.fn_info_mut(def).state = BodyState::Done;
    recursion::completed(cx, def, inferred);
}

/// Drops the diagnostics from `from` on at a place reported before (a static method's copy
/// repeats the method's errors, naming the subclass where the method names its own class).
/// The ones kept say which copy they come from.
fn drop_repeated_diags(cx: &mut Ctx, def: DefId, from: usize) {
    let new = cx.diags.split_off(from.min(cx.diags.len()));
    let place = |e: &Diagnostic| e.labels.first().map(|l| l.span);
    let note = super::expr::static_copy::copy_note(cx, def);
    for mut d in new {
        if !cx.diags.iter().any(|e| place(e) == place(&d)) {
            if let Some(n) = note.as_ref().filter(|n| !d.notes.contains(n)) {
                d.notes.push(n.clone());
            }
            cx.diags.push(d);
        }
    }
}

fn check_body(cx: &mut Ctx, def: DefId, src: FnSource) -> hir::FnDef {
    cx.checking.push(def);
    let in_return = std::mem::take(&mut cx.rec.in_return);
    let names = cx.fn_info(def).generics.names.clone();
    let saved = std::mem::replace(&mut cx.display_params, names);
    let fndef = check_fn(cx, def, src);
    cx.display_params = saved;
    cx.rec.in_return = in_return;
    cx.checking.pop();
    fndef
}

fn check_fn(cx: &mut Ctx, def: DefId, src: FnSource) -> hir::FnDef {
    // An unannotated override takes the result of the method it overrides.
    let declared = match cx.fn_info(def).ret_source {
        RetSource::Body => None,
        _ => Some(super::returns::ret_of(cx, def, cx.fn_info(def).name_span)),
    };
    let f = cx.fn_info(def).clone();
    let mut env = TyEnv::new(f.module, &f.generics.names);
    env.this_ty = f.this_type();
    // An async body returns the promise's payload, which is also the HIR `ret` (the signature,
    // `FnInfo::ret`, and the type of a call stay `Promise<T>`); `None` while it is inferred.
    let body_ret = declared.map(|r| if f.is_async { cx.ty.async_result(r) } else { r });
    // A generator's body yields `T` of its declared `Generator<T>` and returns nothing; the HIR
    // `ret` stays the declared result (hir_encodings.md "Generators"). Its result is never
    // inferred (`collect::sigs`).
    let yield_ty = f.is_generator.then(|| {
        cx.generator_result(f.ret)
            .and_then(|(_, a)| a.first().copied())
            .unwrap_or(cx.ty.error)
    });
    let frame_ret = if yield_ty.is_some() {
        Some(cx.ty.unit)
    } else {
        body_ret
    };
    let mut frame = Frame::new(f.kind, frame_ret);
    frame.is_async = f.is_async || f.is_async_gen;
    frame.top_level = f.is_async
        && f.module == cx.root
        && (f.name == "main" || f.name == velt_syntax::SCRIPT_MAIN)
        && f.name_span.lo == f.name_span.hi;
    frame.yield_ty = yield_ty;
    if yield_ty.is_some() {
        let n = cx.generator_next_ty(f.ret);
        frame.next_ty = (!cx.ty.is_bottom(n)).then_some(n);
    }
    let enclosing_locals = cx
        .nested_locals
        .get(&def)
        .or_else(|| f.owner.and_then(|o| cx.nested_locals.get(&o)))
        .cloned()
        .unwrap_or_default();
    let body = match src {
        FnSource::Decl(d) => &d.body,
        FnSource::Default(_, b) => b,
    };
    frame.scopes[0].hi = body.span.hi;
    let defaults = match src {
        FnSource::Decl(d) => d
            .sig
            .params
            .iter()
            .filter_map(|p| p.default.as_ref())
            .collect(),
        FnSource::Default(..) => vec![],
    };
    frame.assign_ends = super::assign_ends::assignment_ends(&body.stmts, defaults.clone());
    let assigned = super::assigned::assigned_by_closures(&body.stmts, defaults);
    frame.closure_assigned = super::closure_assigned::owned(assigned);
    let decided = cx.literal_types.get(&def).cloned().unwrap_or_default();
    let mut fcx = FnCx::new(cx, f.module, env, frame);
    fcx.literal.decided = decided;
    fcx.bounds = f.generics.bounds.clone();
    fcx.constraints = f.generics.constraints.clone();
    fcx.fn_name = f.name.clone();
    fcx.owner = f.owner;
    fcx.body_def = Some(def);
    fcx.static_this = fcx.cx.static_this.get(&def).map(|&this| {
        let method = fcx.cx.static_copy_of.get(&def).copied().unwrap_or(def);
        (this, fcx.cx.static_this[&method])
    });
    fcx.this_primitive = super::expr::primitive_this(fcx.cx, def);
    fcx.enclosing_locals = enclosing_locals;
    fcx.generic_arrow = fcx.cx.generic_arrow_fns.contains(&f.name_span);
    let params = fcx.declare_params(&f);
    let mut stmts = vec![];
    if let (FnKind::Ctor, FnSource::Decl(d)) = (f.kind, src) {
        fcx.ctor_begin(&f, d);
    }
    let diags_before = fcx.cx.diags.len();
    let sig_params = &crate::body::defaults::fn_sig_ast(src).params;
    if !f.is_generator {
        fcx.default_prologue(
            sig_params
                .iter()
                .zip(&f.params)
                .map(|(p, q)| (&p.name, p.default.as_ref(), p.optional, q.ty)),
            &mut stmts,
        );
    }
    fcx.destructure_params(
        sig_params
            .iter()
            .map(|p| (&p.name, p.pattern.as_ref(), p.optional)),
        &mut stmts,
    );
    fcx.stmts_into(&body.stmts, &mut stmts);
    let mut block = hir::Block {
        stmts,
        value: None,
        span: body.span,
    };
    fcx.reused_assign_targets(&mut block);
    if f.kind == FnKind::Ctor {
        fcx.check_ctor(&f, &block);
    }
    let frame_ret = match frame_ret {
        Some(r) => r,
        None => fcx.inferred_fn_ret(def, &mut block),
    };
    fcx.check_returns(&f.name, frame_ret, f.name_span, &mut block, false);
    fcx.wrap_adopted(&mut block);
    fcx.rec_frame_scopes();
    fcx.finish_using_shares();
    fcx.note_refused_facts(diags_before);
    let frame = std::mem::replace(&mut fcx.f, Frame::new(f.kind, None));
    let found = fcx.literal_take();
    cx.literal_found.insert(def, found);
    let info = cx.fn_info_mut(def);
    info.local_kinds = frame.kinds;
    info.throw_srcs = frame.uncaught;
    info.soft_moves = frame.soft_moves;
    hir::FnDef {
        name: f.name.clone(),
        generics: f.generics.len() as u32,
        params,
        ret: body_ret.unwrap_or(frame_ret),
        is_async: f.is_async || f.is_async_gen,
        is_generator: f.is_generator,
        // A function's `this` parameter is no method's receiver.
        self_ty: f
            .this
            .as_ref()
            .filter(|_| !f.has_this_param())
            .map(|t| t.ty),
        captures: vec![],
        shares_captures: false,
        body: hir::Body {
            locals: frame.locals,
            block,
        },
        throws: None,
        span: f.span,
    }
}

impl FnCx<'_, '_> {
    /// `this` and the declared params as the first locals of the body.
    fn declare_params(&mut self, f: &crate::defs::FnInfo) -> Vec<hir::Param> {
        let mut params = vec![];
        if let Some(t) = &f.this {
            let l = self.new_local("this", t.ty, false, f.name_span, LocalKind::This);
            self.f.scopes[0].names.insert("this".into(), l);
            params.push(hir::Param {
                local: l,
                ty: t.ty,
                mode: t.mode,
            });
        }
        for p in &f.params {
            // `mutable` is set when the body assigns or modifies the param (`mark_mutated`).
            let l = self.new_local(&p.name, p.ty, false, p.span, LocalKind::Param);
            if let Some(crate::defs::FnSource::Decl(d)) = f.source {
                let written = d.sig.params.iter().find(|q| q.name.name == p.name);
                self.note_marks(l, written.map(|q| &q.ty), None);
            }
            self.f.scopes[0].names.entry(p.name.clone()).or_insert(l);
            self.rec_local_decl(l);
            params.push(hir::Param {
                local: l,
                ty: p.ty,
                mode: p.mode,
            });
        }
        params
    }

    /// The result of function `def` inferred from its checked body `block`, recorded as its
    /// signature (`Promise<T>` for an async function).
    fn inferred_fn_ret(&mut self, def: DefId, block: &mut hir::Block) -> hir::TyId {
        let ret = self.finish_inferred_ret(block);
        let sig = if self.f.is_async {
            self.cx.ty.promise(ret)
        } else {
            ret
        };
        let info = self.cx.fn_info_mut(def);
        info.ret = sig;
        info.ret_source = RetSource::Known;
        ret
    }

    /// "must return a value on every path" for non-void functions. A function whose result may
    /// be `undefined` returns it at the end, as in TypeScript (issue #930).
    pub fn check_returns(
        &mut self,
        name: &str,
        ret: hir::TyId,
        span: Span,
        block: &mut hir::Block,
        from_context: bool,
    ) {
        // A function declared to return `never` must not reach its end (TS2534): its callers
        // take it as never returning, so a reachable end would return where nothing follows.
        if ret == self.cx.ty.never
            && !self.f.is_async
            && self.f.yield_ty.is_none()
            && !crate::flow::block_diverges(block, &self.cx.ty)
        {
            let short = name.rsplit("::").next().unwrap_or(name);
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "function `{short}` returns `never`, so it cannot have a reachable end point"
                    ),
                    span,
                )
                .with_note("throw, loop forever or call a `never` function on every path"),
            );
            return;
        }
        if ret != self.cx.ty.unit
            && !self.cx.ty.is_bottom(ret)
            && !crate::flow::block_diverges(block, &self.cx.ty)
        {
            if self
                .cx
                .ty
                .nothing_of(ret)
                .is_some_and(|n| n.has_undefined())
            {
                let end = Span::new(block.span.file, block.span.hi, block.span.hi);
                let value = self.mk(hir::ExprKind::Lit(hir::Lit::Null), ret, end);
                block.stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Return(Some(value)),
                    span: end,
                });
                return;
            }
            // A function declared to return `any`, or an arrow typed by its context to return
            // `any` or `unknown` (sigx's `WatchCallback` returns `any`), returns `undefined` at
            // its end, as in TypeScript; a function declared `unknown` must return (TS2355).
            let dynamic = match self.cx.dynamic(ret) {
                Some(crate::any::Dynamic::Any) => true,
                Some(crate::any::Dynamic::Unknown) => from_context,
                _ => false,
            };
            if dynamic {
                let end = Span::new(block.span.file, block.span.hi, block.span.hi);
                let undefined = ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::ExprKind::Lit(ast::Lit::Undefined),
                    span: end,
                };
                let value = self.expr(&undefined, Some(ret), Want::Move);
                let value = self.coerce(value, ret);
                block.stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Return(Some(value)),
                    span: end,
                });
                return;
            }
            let rty = self.cx.display(ret);
            let short = name.rsplit("::").next().unwrap_or(name);
            self.cx.error(
                Diagnostic::error(
                    format!("function `{short}` must return a value of type `{rty}` on every path"),
                    span,
                )
                .with_note(format!("expected {rty}, found void")),
            );
        }
    }
}
