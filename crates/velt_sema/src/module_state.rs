//! Module state (#76): module-level `let`s become slots of one process-wide tuple
//! (hir_encodings.md "Module state"). Slots are numbered in Node's module evaluation order: a
//! depth-first walk over each module's imports in source order, a module after the modules it
//! imports, and its own declarations in source order. The state's initializer evaluates them
//! in that order, so an initializer that always reads a `let` whose initializer has not run yet
//! is an error, as Node throws a `ReferenceError` there (a read only some runs reach stops with
//! that error at run time).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as E, TyKind};

#[path = "module_state_reads.rs"]
mod always;
#[path = "module_state_facts.rs"]
mod facts;
#[path = "locked_props.rs"]
mod locked_props;
#[path = "module_state_reach.rs"]
mod reach;

use facts::Facts;
pub(crate) use facts::StateFacts;
use reach::{propagate, vars_reached, Reach};

/// The module-level `let`s in initialization order (empty when there are none); reports reads
/// of a `let` before its initializer runs.
pub(crate) fn order(cx: &mut Ctx) -> Vec<DefId> {
    crate::linked::keep_edges(cx);
    // In a check by units, std is held whole: a std module the program does not link has no
    // state when it runs (`crate::linked`).
    let linked = crate::linked::linked(cx);
    let lets: Vec<(usize, u32, DefId)> = cx
        .info
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match d {
            // `import.meta` exists before the module's code runs (at its first use's position).
            DefInfo::Global(g) if g.stored => {
                let start = match g.name == velt_syntax::ast::IMPORT_META {
                    true => 0,
                    false => g.src.span.lo,
                };
                Some((g.module, start, DefId(i as u32)))
            }
            _ => None,
        })
        .filter(|(m, ..)| linked[*m] || !cx.modules[*m].is_std)
        .collect();
    if lets.is_empty() {
        return Vec::new();
    }
    let class_values = class_value_set(cx);
    // The classes' values exist before any of their module's code runs (they read nothing,
    // `body::expr::ctor_values`, `ctor_objects`): at its start, also when the root file's
    // slots are initialized among its statements (`defer_root`), which go by these positions.
    let lets: Vec<(usize, u32, DefId)> = lets
        .into_iter()
        .map(|(m, start, d)| (m, if class_values.contains(&d) { 0 } else { start }, d))
        .collect();
    let (modules, evaluated, reached) = module_order(cx);
    let rank: FxHashMap<usize, usize> = modules.iter().enumerate().map(|(i, m)| (*m, i)).collect();
    // A module no value import reaches is never evaluated: its top-level statements don't run,
    // and neither do the initializers of a module imported only for its types. (A module no
    // import reaches, such as the standard library's, keeps its slots: code may use them.)
    let skipped: FxHashSet<DefId> = lets
        .iter()
        .filter(|(m, _, d)| {
            !evaluated[*m]
                && (crate::module_scripts::is_script(cx, *d)
                    || (reached[*m] && !cx.modules[*m].is_std))
        })
        .map(|(_, _, d)| *d)
        .collect();
    let skipped = unread_elsewhere(cx, skipped);
    let mut sorted: Vec<_> = lets
        .into_iter()
        .filter(|(_, _, d)| !skipped.contains(d))
        .collect();
    // The properties of `globalThis` exist before any module runs (`crate::global_this`), and
    // so do the classes' values, which read nothing (`body::expr::ctor_values`).
    sorted.sort_by_key(|(m, start, d)| {
        let rank = rank.get(m).copied().unwrap_or(usize::MAX);
        let first = cx.global_props.is_slot(*d) || class_values.contains(d);
        (!first, rank, *start)
    });
    let order: Vec<DefId> = sorted.into_iter().map(|(_, _, d)| d).collect();
    let pos: FxHashMap<DefId, usize> = order.iter().enumerate().map(|(i, d)| (*d, i)).collect();
    for (i, &d) in order.iter().enumerate() {
        let Some(g) = cx.global(d) else { continue };
        let Some(init) = g.init.as_ref() else {
            continue;
        };
        // `export default expr;` is the constant `default` (#1916).
        let shown = |n: String| match n.as_str() {
            "default" => "the default export".to_string(),
            _ => format!("`{n}`"),
        };
        let name = shown(g.name.clone());
        let mut early = Vec::new();
        // Only a read that always runs: any other stops at run time if it does run.
        always::reads(init, &mut |r, span| {
            if pos.get(&r).is_some_and(|&j| j >= i) {
                early.push((r, span));
            }
        });
        for (r, span) in early {
            // `export default x;` above `x`: `script_tdz` said so already.
            let said = |d: &Diagnostic| d.labels.first().is_some_and(|l| l.span == span);
            if cx.diags.iter().any(said) {
                continue;
            }
            let other = shown(cx.global(r).map(|g| g.name.clone()).unwrap_or_default());
            cx.error(
                Diagnostic::error(format!("{other} is read before its initializer runs"), span)
                    .with_note(format!(
                        "module state is initialized in order, and {name}'s initializer \
                         runs first (JavaScript throws a `ReferenceError` here)"
                    )),
            );
        }
    }
    order
}

/// Module indices in evaluation order: those reachable from the root by a post-order walk over
/// value imports, then any others (which have no code that runs first); per module, whether
/// the walk reached it (whether Node evaluates it); and whether any import reaches it.
fn module_order(cx: &Ctx) -> (Vec<usize>, Vec<bool>, Vec<bool>) {
    let by_path: FxHashMap<&str, usize> = cx
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let mut seen = vec![false; cx.modules.len()];
    let mut out = Vec::new();
    // Iterative post-order: (module, next import to visit).
    let mut stack = vec![(cx.root, 0usize)];
    seen[cx.root] = true;
    while let Some((m, next)) = stack.pop() {
        let module = &cx.modules[m];
        if let Some((spec, path)) = module.imports.get(next) {
            stack.push((m, next + 1));
            if let Some(&dep) = by_path.get(path.as_str()) {
                if !seen[dep] && evaluates(module, spec) {
                    seen[dep] = true;
                    stack.push((dep, 0));
                }
            }
        } else {
            out.push(m);
        }
    }
    let root = out.pop();
    for (m, s) in seen.iter().enumerate() {
        if !s {
            out.push(m);
        }
    }
    out.extend(root);
    let mut reached = vec![false; cx.modules.len()];
    reached[cx.root] = true;
    let mut todo = vec![cx.root];
    while let Some(m) = todo.pop() {
        for (_, path) in &cx.modules[m].imports {
            if let Some(&dep) = by_path.get(path.as_str()) {
                if !reached[dep] {
                    reached[dep] = true;
                    todo.push(dep);
                }
            }
        }
    }
    (out, seen, reached)
}

/// The slots of `skipped` that no code that may run reads: none of another module's code,
/// and no interface's default method (which an object of another module may run).
fn unread_elsewhere(cx: &mut Ctx, mut skipped: FxHashSet<DefId>) -> FxHashSet<DefId> {
    if skipped.is_empty() {
        return skipped;
    }
    let module_of = |cx: &Ctx, d: DefId| cx.global(d).map(|g| g.module);
    let mut read: Vec<(DefId, usize, bool)> = vec![];
    for i in 0..cx.info.len() {
        let (module, iface_default, e) = match (&cx.info[i], cx.defs.get(i)) {
            (DefInfo::Fn(f), Some(Some(hir::Def::Fn(def)))) => (
                f.module,
                f.kind == crate::defs::FnKind::IfaceDefault,
                hir::Expr {
                    kind: E::Block(def.body.block.clone()),
                    ty: cx.ty.unit,
                    span: velt_common::Span::DUMMY,
                },
            ),
            (DefInfo::Global(g), _) => match &g.init {
                Some(init) => (g.module, false, init.clone()),
                None => continue,
            },
            _ => continue,
        };
        reads(&e, &mut |r, _| {
            if skipped.contains(&r) {
                read.push((r, module, iface_default));
            }
        });
    }
    for (r, module, iface_default) in read {
        if iface_default || module_of(cx, r) != Some(module) {
            skipped.remove(&r);
        }
    }
    skipped
}

/// Does `module` import `spec` for more than its types: an import or re-export of `spec` that
/// TypeScript keeps (not `import type { … }`, nor one whose every name is `type`), or a
/// side-effect `import "spec"`?
fn evaluates(module: &crate::SourceModule, spec: &str) -> bool {
    module.ast.items.iter().any(|it| match &it.kind {
        velt_syntax::ast::ItemKind::Import(imp) if imp.from == spec => {
            !imp.type_only && imp.namespace.is_some()
                || imp.all
                || (imp.names.is_empty() && !imp.type_only)
                || imp.names.iter().any(|n| !n.type_only)
        }
        _ => false,
    })
}

/// Does `e` read a module `let` directly?
pub(crate) fn reads_state(cx: &Ctx, e: &hir::Expr) -> bool {
    let mut found = false;
    reads(e, &mut |d, _| {
        found |= cx.global(d).is_some_and(|g| g.mutable);
    });
    found
}

/// Every module global `e` reads directly.
fn reads(e: &hir::Expr, f: &mut dyn FnMut(DefId, velt_common::Span)) {
    let mut e = e.clone();
    struct V<'a>(&'a mut dyn FnMut(DefId, velt_common::Span));
    impl crate::visit::VisitMut for V<'_> {
        fn expr(&mut self, e: &mut hir::Expr) {
            if let E::Global(d) = e.kind {
                (self.0)(d, e.span);
            }
        }
    }
    crate::visit::expr(&mut e, &mut V(f));
}

/// The program's module state, with each slot's global marked (`GlobalDef::is_slot`); the root
/// file's slots are initialized by its top-level statements (`entry`, [`defer_root`]).
pub(crate) fn build(cx: &mut Ctx, entry: Option<DefId>) -> Option<hir::ModuleState> {
    if cx.module_slots.is_empty() {
        return None;
    }
    let slots = cx.module_slots.clone();
    let mut tys = Vec::with_capacity(slots.len());
    for &d in &slots {
        let Some(Some(hir::Def::Global(g))) = cx.defs.get_mut(d.0 as usize) else {
            panic!("ICE: module state slot is not a global");
        };
        g.is_slot = true;
        tys.push(g.ty);
    }
    let ty = cx.ty.intern(TyKind::Tuple(tys));
    let deferred = defer_root(cx, entry, &slots);
    Some(hir::ModuleState {
        ty,
        slots,
        deferred,
    })
}

/// In Node the root file's declarations are evaluated in source order with its top-level
/// statements (#1438): `g.x = 6; class C { static readonly Y = g.x }` gives `C.Y` 6. So when the
/// entry `main` holds the root file's statements, its slots (the last ones: the root module is
/// evaluated last) are initialized there, each run of them before the first statement that
/// follows it ([`hir::Intrinsic::InitSlots`]); a statement reaching a slot that is not
/// initialized yet stops with Node's `ReferenceError`, as any read before initialization does.
/// The first deferred slot (`slots.len()` when none is).
fn defer_root(cx: &mut Ctx, entry: Option<DefId>, slots: &[DefId]) -> u32 {
    let none = slots.len() as u32;
    let Some(main) = entry.filter(|_| crate::module_scripts::is_root_script(cx, cx.root)) else {
        return none;
    };
    let is_root = |d: DefId| {
        cx.global(d)
            .is_some_and(|g| g.module == cx.root && g.name != velt_syntax::ast::IMPORT_META)
            && !cx.global_props.is_slot(d)
    };
    let Some(from) = slots.iter().position(|&d| is_root(d)) else {
        return none;
    };
    if !slots[from..].iter().all(|&d| is_root(d)) {
        return none;
    }
    let class_values = class_value_set(cx);
    let starts: Vec<u32> = slots[from..]
        .iter()
        .map(|&d| match class_values.contains(&d) {
            // Before the file's first statement (`order`).
            true => 0,
            false => cx.global(d).map_or(0, |g| g.src.span.lo),
        })
        .collect();
    let unit = cx.ty.unit;
    let Some(Some(hir::Def::Fn(f))) = cx.defs.get_mut(main.0 as usize) else {
        return none;
    };
    let stmts = std::mem::take(&mut f.body.block.stmts);
    let mut out = Vec::with_capacity(stmts.len() + 2);
    let mut next = 0;
    let init_to = |out: &mut Vec<hir::Stmt>, next: &mut usize, upto: Option<u32>| {
        let start = *next;
        while *next < starts.len() && upto.is_none_or(|lo| starts[*next] < lo) {
            *next += 1;
        }
        if *next > start {
            let (a, b) = ((from + start) as u32, (from + *next) as u32);
            let span = velt_common::Span::DUMMY;
            let call = hir::Expr {
                kind: E::Call {
                    callee: hir::Callee::Intrinsic(hir::Intrinsic::InitSlots(a, b)),
                    args: vec![],
                },
                ty: unit,
                span,
            };
            out.push(hir::Stmt {
                kind: hir::StmtKind::Expr(call),
                span,
            });
        }
    };
    for s in stmts {
        // Statements the checker adds have no position of their own.
        if s.span.lo != s.span.hi || s.span.lo != 0 {
            init_to(&mut out, &mut next, Some(s.span.lo));
        }
        out.push(s);
    }
    init_to(&mut out, &mut next, None);
    f.body.block.stmts = out;
    from as u32
}

impl crate::body::FnCx<'_, '_> {
    /// `x = v` where `x` names a module-level `let` (or a kept `const`, which `require_mutable`
    /// rejects): its slot, or `Some(None)` after an error; `None` for any other name.
    pub(crate) fn module_let_target(
        &mut self,
        item: Option<crate::ctx::Item>,
        id: &velt_syntax::ast::Ident,
    ) -> Option<Option<hir::Expr>> {
        let Some(crate::ctx::Item::Def(d)) = item else {
            return None;
        };
        // A C variable is assigned in C memory (`c_global_write` says whether it may be).
        if !self
            .cx
            .global(d)
            .is_some_and(|g| g.stored || g.src.c_global.is_some())
        {
            return None;
        }
        // `export default expr;` is the constant `default`, a name the source doesn't have.
        if self.cx.global(d).is_some_and(|g| g.name == "default") {
            self.cx.err(
                format!("cannot assign to `{}`, an import", id.name),
                id.span,
            );
            return Some(None);
        }
        let place = self.global_read(d, crate::body::Want::BorrowMut, id.span);
        Some(self.require_mutable(&place, "assign to").then_some(place))
    }
}

/// A `Mutex` is locked by blocking the thread, while a task holds the module lock for its whole
/// step: a `with` callback that reaches module state could wait for the module lock while holding
/// the `Mutex` another thread waits for, holding the module lock. So module state is not
/// reachable from a `with` callback: not directly, not through the functions it calls, not
/// through the code that runs without a call (field and constant initializers, and the
/// `[Symbol.dispose]()`, `toString()`, `toJSON()`, `[INSPECT]()` and `clone()` hooks that drops
/// and glue run: `replaced_holders::implicit` lists them), and not through a function value whose
/// body is not known (rt_abi.md "Module state"; the runtime stops with an `ICE:` otherwise).
/// Every variable a refused callback reaches is recorded (`script_locals`; through a function
/// value it calls, those of the closures the code around it makes: `reach::vars_reached`).
pub(crate) fn check_locked_access(cx: &mut Ctx) {
    // Without `any`, no function has properties (locked_props.rs).
    let props = cx.any.is_some_and(|a| a.imp.is_some());
    // std's stage reads its bodies' facts for the app's check, whatever its own needs
    // (`facts::scan`).
    let for_later = cx.units_stage == crate::units::Stage::Std;
    if cx.module_slots.is_empty() && !props && !for_later {
        return;
    }
    locked_props::reset();
    let dispatch = locked_props::Dispatch::new(cx);
    let facts::Scanned {
        mut facts,
        withs,
        callbacks,
        proxies,
    } = facts::scan(cx, &dispatch);
    if cx.module_slots.is_empty() && !props {
        return;
    }
    if withs.is_empty() && callbacks.is_empty() && cx.c_exports.is_empty() {
        return;
    }
    // The hooks the values a body handles may run when dropped, formatted or serialised, and
    // the generator bodies a resume may run.
    let weak = crate::replaced_holders::implicit::weak_entries(cx);
    let im = crate::replaced_holders::implicit::Implicit::new(cx, weak);
    for f in facts.values_mut() {
        let mut hooks: Vec<DefId> = vec![];
        for &t in &f.tys {
            hooks.extend(im.hooks_reached(cx, t));
        }
        if f.resumes {
            hooks.extend(im.generators.iter().copied());
        }
        hooks.sort();
        hooks.dedup();
        f.edges.extend(hooks);
    }
    if props {
        let bodies = (facts.values_mut()).map(|f| (&f.tys, &mut f.props));
        proxies.reach(cx, bodies);
        let graph = (facts.iter())
            .map(|(&d, f)| (d, (f.props, &f.edges[..], &f.slots[..])))
            .collect();
        cx.lock_free_callbacks = locked_props::lock_free(cx, &dispatch, &graph, &callbacks);
    }
    if cx.module_slots.is_empty() {
        return;
    }
    let reach = propagate(&facts);
    reach::refuse_in_exports(cx, &reach);
    let mut refused = vec![];
    for (cb, span, method) in withs {
        let found = match &cb.kind {
            E::Closure(c) => reach.get(c).cloned(),
            E::FnRef(c, _) => reach.get(c).cloned().map(|mut r| {
                r.via.insert(0, *c);
                r
            }),
            _ => Some(Reach {
                what: "is a function value whose body is not known here".into(),
                via: vec![],
            }),
        };
        let Some(r) = found else { continue };
        if let E::Closure(c) | E::FnRef(c, _) = &cb.kind {
            refused.push(*c);
        }
        let what = match r.via.is_empty() {
            true => r.what,
            false => {
                let chain: Vec<String> = r.via.iter().map(|&f| node_name(cx, f)).collect();
                format!("{} (through {})", r.what, chain.join(" -> "))
            }
        };
        cx.error(
            Diagnostic::error(
                format!("the function passed to `{method}` may use module state: it {what}"),
                span,
            )
            .with_note(format!("`{method}` blocks the thread while it waits for the lock, so module-level `let`s and kept constants cannot be used under it: two tasks could wait for each other"))
            .with_note(format!("read what you need before `{method}` and pass it in, and store what it returns after")),
        );
    }
    cx.state_refused.extend(vars_reached(&facts, refused));
}

/// How a diagnostic names a node of the reach graph: a function, an object type's field
/// initializers or a constant's initializer.
pub(super) fn node_name(cx: &Ctx, d: DefId) -> String {
    let last = |n: &str| n.rsplit("::").next().unwrap_or(n).to_string();
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Fn(f)) => format!("`{}`", last(&f.name)),
        Some(DefInfo::Adt(a)) => format!("the field initializers of `{}`", a.name),
        Some(DefInfo::Global(g)) => format!("the initializer of `{}`", g.name),
        _ => "code that runs without a call".into(),
    }
}

/// The constants holding classes' values (`body::expr::ctor_values`, `ctor_objects`).
fn class_value_set(cx: &Ctx) -> FxHashSet<DefId> {
    cx.objects.class_values.iter().map(|(_, g)| *g).collect()
}
