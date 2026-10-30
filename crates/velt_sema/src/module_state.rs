//! Module state (#76): module-level `let`s become slots of one process-wide tuple
//! (hir_encodings.md "Module state"). Slots are numbered in Node's module evaluation order: a
//! depth-first walk over each module's imports in source order, a module after the modules it
//! imports, and its own declarations in source order. The state's initializer evaluates them
//! in that order, so an initializer that reads a `let` whose initializer has not run yet is an
//! error, as Node throws a `ReferenceError` there.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as E, TyId, TyKind};

#[path = "locked_props.rs"]
mod locked_props;

/// The module-level `let`s in initialization order (empty when there are none); reports reads
/// of a `let` before its initializer runs.
pub(crate) fn order(cx: &mut Ctx) -> Vec<DefId> {
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
        let name = g.name.clone();
        let mut early = Vec::new();
        reads(init, &mut |r, span| {
            if pos.get(&r).is_some_and(|&j| j >= i) {
                early.push((r, span));
            }
        });
        for (r, span) in early {
            let other = cx.global(r).map(|g| g.name.clone()).unwrap_or_default();
            cx.error(
                Diagnostic::error(
                    format!("`{other}` is read before its initializer runs"),
                    span,
                )
                .with_note(format!(
                    "module state is initialized in order, and `{name}`'s initializer \
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
            imp.namespace.is_some()
                || imp.all
                || imp.names.is_empty()
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

/// The program's module state, with each slot's global numbered (`GlobalDef::slot`); the root
/// file's slots are initialized by its top-level statements (`entry`, [`defer_root`]).
pub(crate) fn build(cx: &mut Ctx, entry: Option<DefId>) -> Option<hir::ModuleState> {
    if cx.module_slots.is_empty() {
        return None;
    }
    let slots = cx.module_slots.clone();
    let mut tys = Vec::with_capacity(slots.len());
    for (i, &d) in slots.iter().enumerate() {
        let Some(Some(hir::Def::Global(g))) = cx.defs.get_mut(d.0 as usize) else {
            panic!("ICE: module state slot is not a global");
        };
        g.slot = Some(i as u32);
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
        if !self.cx.global(d).is_some_and(|g| g.stored) {
            return None;
        }
        let place = self.global_read(d, crate::body::Want::BorrowMut, id.span);
        Some(self.require_mutable(&place, "assign to").then_some(place))
    }
}

/// Why a function may reach module state: the `let` or kept `const` it uses (and which), or that
/// it calls a function value whose body is not known; with the calls that lead there.
#[derive(Clone)]
struct Reach {
    what: (String, Option<DefId>),
    via: Vec<DefId>,
}

/// What one body does with module state: its own first use, and the code it may run (calls,
/// closures it makes, functions it takes as values, the methods a dynamic call may pick, the
/// initializers a `new` or a constant read evaluates); and the types of the values it handles,
/// whose class hooks it may run without a call (`replaced_holders::implicit`).
struct Facts {
    direct: Option<(String, Option<DefId>)>,
    edges: Vec<DefId>,
    tys: FxHashSet<TyId>,
    /// It resumes a generator: any generator body may run.
    resumes: bool,
    /// It may reach a function's properties by itself (locked_props.rs).
    props: bool,
    /// The interface slots it calls through (`(iface, slot)`): their implementations are its
    /// edges only when module state is checked; the walk for properties goes through the slot
    /// once instead (locked_props.rs), so many calls of many implementations stay linear.
    slots: Vec<(DefId, u32)>,
}

/// A `Mutex` is locked by blocking the thread, while a task holds the module lock for its whole
/// step: a `with` callback that reaches module state could wait for the module lock while holding
/// the `Mutex` another thread waits for, holding the module lock. So module state is not
/// reachable from a `with` callback: not directly, not through the functions it calls, not
/// through the code that runs without a call (field and constant initializers, and the
/// `[Symbol.dispose]()`, `toString()`, `toJSON()`, `__inspect()` and `clone()` hooks that drops
/// and glue run: `replaced_holders::implicit` lists them), and not through a function value whose
/// body is not known (rt_abi.md "Module state"; the runtime stops with an `ICE:` otherwise).
/// A variable this refuses is recorded (`script_locals`).
pub(crate) fn check_locked_access(cx: &mut Ctx) {
    // Without `any`, no function has properties (locked_props.rs).
    let props = cx.any.is_some_and(|a| a.imp.is_some());
    if cx.module_slots.is_empty() && !props {
        return;
    }
    locked_props::reset();
    let dispatch = locked_props::Dispatch::new(cx);
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == crate::defs::BodyState::Done)
        .collect();
    let mut withs: Vec<(hir::Expr, velt_common::Span, &'static str)> = vec![];
    let mut callbacks: Vec<DefId> = vec![];
    let mut proxies = locked_props::Proxies::default();
    let mut facts: FxHashMap<DefId, Facts> = FxHashMap::default();
    for &d in &fns {
        let std = cx.modules[cx.fn_info(d).module].is_std;
        let Some(hir::Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut v = FactsOf::new(cx, &dispatch, std);
        if !std {
            v.facts.tys.extend(f.body.locals.iter().map(|l| l.ty));
        }
        crate::visit::block(&mut f.body.block, &mut v);
        let (fd, ws) = (v.facts, v.withs);
        callbacks.extend(v.callbacks);
        proxies.add(v.proxies);
        cx.defs[d.0 as usize] = Some(hir::Def::Fn(f));
        if !std {
            withs.extend(ws);
        }
        facts.insert(d, fd);
    }
    // Initializers evaluated where they are used (a constant's at each read, an object type's
    // field initializers at each `new` or literal): each a node of its own, under the
    // constant's or the type's `DefId`.
    for i in 0..cx.info.len() {
        let (module, inits): (usize, Vec<hir::Expr>) = match &cx.info[i] {
            DefInfo::Global(g) if !g.stored => (g.module, g.init.iter().cloned().collect()),
            DefInfo::Adt(a) => (
                a.module,
                a.fields
                    .iter()
                    .filter_map(|f| f.default.clone())
                    .chain(a.redefined.iter().filter_map(|r| r.init.clone()))
                    .collect(),
            ),
            _ => continue,
        };
        if inits.is_empty() {
            continue;
        }
        let std = cx.modules.get(module).is_some_and(|m| m.is_std);
        let mut v = FactsOf::new(cx, &dispatch, std);
        for mut x in inits {
            crate::visit::expr(&mut x, &mut v);
        }
        let (fd, ws) = (v.facts, v.withs);
        callbacks.extend(v.callbacks);
        if !std {
            withs.extend(ws);
        }
        facts.insert(DefId(i as u32), fd);
    }
    if withs.is_empty() && callbacks.is_empty() {
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
    for (cb, span, method) in withs {
        let found = match &cb.kind {
            E::Closure(c) => reach.get(c).cloned(),
            E::FnRef(c, _) => reach.get(c).cloned().map(|mut r| {
                r.via.insert(0, *c);
                r
            }),
            _ => Some(Reach {
                what: (
                    "is a function value whose body is not known here".into(),
                    None,
                ),
                via: vec![],
            }),
        };
        let Some(r) = found else { continue };
        cx.state_refused.extend(r.what.1);
        let what = match r.via.is_empty() {
            true => r.what.0,
            false => {
                let chain: Vec<String> = r.via.iter().map(|&f| node_name(cx, f)).collect();
                format!("{} (through {})", r.what.0, chain.join(" -> "))
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
}

/// How a diagnostic names a node of the reach graph: a function, an object type's field
/// initializers or a constant's initializer.
fn node_name(cx: &Ctx, d: DefId) -> String {
    let last = |n: &str| n.rsplit("::").next().unwrap_or(n).to_string();
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Fn(f)) => format!("`{}`", last(&f.name)),
        Some(DefInfo::Adt(a)) => format!("the field initializers of `{}`", a.name),
        Some(DefInfo::Global(g)) => format!("the initializer of `{}`", g.name),
        _ => "code that runs without a call".into(),
    }
}

/// Which functions may reach module state: those that use it, and those that may run one of
/// them (backwards along the edges, to a fixpoint).
fn propagate(facts: &FxHashMap<DefId, Facts>) -> FxHashMap<DefId, Reach> {
    let mut reach: FxHashMap<DefId, Reach> = FxHashMap::default();
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    let mut todo = vec![];
    // In `DefId` order, so a diagnostic names the same path on every run.
    let mut order: Vec<(&DefId, &Facts)> = facts.iter().collect();
    order.sort_by_key(|(d, _)| **d);
    for (&d, f) in order {
        for &e in &f.edges {
            callers.entry(e).or_default().push(d);
        }
        if let Some(what) = &f.direct {
            let r = Reach {
                what: what.clone(),
                via: vec![],
            };
            reach.insert(d, r);
            todo.push(d);
        }
    }
    while let Some(d) = todo.pop() {
        let r = reach[&d].clone();
        for &c in callers.get(&d).map(Vec::as_slice).unwrap_or(&[]) {
            if reach.contains_key(&c) {
                continue;
            }
            let mut via = vec![d];
            via.extend(r.via.iter().copied());
            let what = r.what.clone();
            reach.insert(c, Reach { what, via });
            todo.push(c);
        }
    }
    reach
}

/// Is `e` a closure or a named function written in place (an optional parameter's `Some`, or
/// either branch of a conditional): a function value whose body the visit sees?
fn written_fn(e: &hir::Expr) -> bool {
    match &e.kind {
        E::Closure(_) | E::FnRef(..) => true,
        E::WrapSome(x) | E::Upcast(x) => written_fn(x),
        E::If { then, els, .. } => written_fn(then) && written_fn(els),
        E::Block(b) if b.stmts.is_empty() => b.value.as_deref().is_some_and(written_fn),
        _ => false,
    }
}

/// Collects a body's [`Facts`] and the callbacks of its `with` calls.
struct FactsOf<'a, 'm> {
    cx: &'a Ctx<'m>,
    dispatch: &'a locked_props::Dispatch,
    std: bool,
    facts: Facts,
    withs: Vec<(hir::Expr, velt_common::Span, &'static str)>,
    /// The callbacks of every `with`, `read` and `write` written as a closure or a named
    /// function, also the standard library's (locked_props.rs).
    callbacks: Vec<DefId>,
    proxies: locked_props::Proxies,
}

impl<'a, 'm> FactsOf<'a, 'm> {
    fn new(cx: &'a Ctx<'m>, dispatch: &'a locked_props::Dispatch, std: bool) -> Self {
        FactsOf {
            cx,
            dispatch,
            std,
            facts: Facts {
                direct: None,
                edges: vec![],
                tys: FxHashSet::default(),
                resumes: false,
                props: false,
                slots: vec![],
            },
            withs: vec![],
            callbacks: vec![],
            proxies: Default::default(),
        }
    }

    fn unknown(&mut self, what: &str) {
        // The standard library only calls the function values it is given: those are made, or
        // taken as values, by the code that passes them, where they count (`passed`).
        if !self.std {
            self.facts.props = true;
            if self.facts.direct.is_none() {
                self.facts.direct = Some((what.into(), None));
            }
        }
    }

    /// Arguments of a call into the standard library or an intrinsic: one that is or holds an
    /// `any` may have its members read, written or listed there (locked_props.rs).
    fn dynamic_args(&mut self, args: &[hir::Expr]) {
        if !self.facts.props {
            let cx = self.cx;
            self.facts.props = args.iter().any(|a| locked_props::holds_dynamic(cx, a.ty));
        }
    }

    /// The arguments of a call into code this analysis does not follow (the standard library,
    /// an intrinsic, or a method a dynamic call picks, which may be std's): a function value it
    /// is handed may run, and only a closure or a named function written there is known.
    fn passed(&mut self, args: &[hir::Expr]) {
        for a in args {
            if !written_fn(a) {
                self.fn_values_in(a.ty, 0);
            }
        }
    }

    /// The function values a value of type `t` holds by itself (not behind an object): a
    /// closure's type names its body, any other is not known.
    fn fn_values_in(&mut self, t: TyId, depth: u32) {
        if depth > 8 {
            return;
        }
        match self.cx.ty.kind(t) {
            TyKind::Closure(c) => self.facts.edges.push(*c),
            TyKind::FnPtr { .. } => self.unknown(
                "passes a function value whose body is not known here to code that may call it",
            ),
            TyKind::Option(x, _) | TyKind::Array(x) | TyKind::Shared(x) => {
                self.fn_values_in(*x, depth + 1)
            }
            TyKind::Tuple(xs) => {
                for &x in xs {
                    self.fn_values_in(x, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Is `d` (a function or an object type) the standard library's?
    fn is_std_def(&self, d: DefId) -> bool {
        let module = match self.cx.info.get(d.0 as usize) {
            Some(DefInfo::Fn(f)) => f.module,
            Some(DefInfo::Adt(a)) => a.module,
            _ => return true,
        };
        self.cx.modules.get(module).is_none_or(|m| m.is_std)
    }

    /// The methods a call through slot `slot` of interface `iface` may run.
    fn iface_methods(&mut self, iface: DefId, slot: u32) {
        self.facts.slots.push((iface, slot));
        if !self.cx.module_slots.is_empty() {
            let ms = self.dispatch.iface_methods(self.cx, iface, slot);
            self.facts.edges.extend(ms);
        }
    }

    /// The methods a virtual call through slot `slot` of class `class` may run (also subclasses').
    fn virtual_methods(&mut self, class: DefId, slot: u32) {
        let ms = self.dispatch.virtual_methods(class, slot);
        self.facts.edges.extend(ms);
    }
}

impl crate::visit::VisitMut for FactsOf<'_, '_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        if !self.std {
            self.facts.tys.insert(e.ty);
            self.proxies.note(e);
        }
        match &e.kind {
            E::Global(d) => match self.cx.global(*d) {
                Some(g) if g.stored => {
                    if self.facts.direct.is_none() {
                        self.facts.direct = Some((format!("uses `{}`", g.name), Some(*d)));
                    }
                }
                // A constant's initializer runs at each read.
                Some(g)
                    if g.init
                        .as_ref()
                        .is_some_and(|x| !matches!(x.kind, E::Lit(_))) =>
                {
                    self.facts.edges.push(*d)
                }
                _ => {}
            },
            E::Closure(c) | E::FnRef(c, _) => self.facts.edges.push(*c),
            E::New { def, args, .. } => {
                let ctor = self.cx.adt(*def).and_then(|a| a.ctor);
                self.facts.edges.extend(ctor);
                // Its field initializers.
                self.facts.edges.push(*def);
                if ctor.is_none_or(|c| self.is_std_def(c)) {
                    self.passed(args);
                    if !ctor.is_some_and(|c| locked_props::has_body(self.cx, c)) {
                        self.dynamic_args(args);
                    }
                }
            }
            E::AdtLit { def, .. } => self.facts.edges.push(*def),
            E::Call { callee, args } => match callee {
                hir::Callee::Def(f, _) => {
                    self.facts.edges.push(*f);
                    if self.is_std_def(*f) {
                        self.passed(args);
                        // A body the walk follows decides by itself (its edge); a runtime
                        // extern may list or print what it gets.
                        if !locked_props::has_body(self.cx, *f) {
                            self.dynamic_args(args);
                        }
                    }
                }
                hir::Callee::Indirect(_) => self.unknown("calls a function value"),
                hir::Callee::Virtual { slot } => {
                    self.passed(args);
                    self.dynamic_args(args);
                    match args.first().and_then(|a| self.cx.class_of(a.ty)) {
                        Some((c, _)) => self.virtual_methods(c, *slot),
                        None => self.unknown("calls a function value"),
                    }
                }
                hir::Callee::Dyn { slot, .. } => {
                    self.passed(args);
                    self.dynamic_args(args);
                    let recv = args.first().map(|a| self.cx.ty.kind(a.ty).clone());
                    match recv {
                        Some(TyKind::Dyn(i, _)) => self.iface_methods(i, *slot),
                        _ => self.unknown("calls a function value"),
                    }
                }
                hir::Callee::ParamMethod { iface, slot, .. } => {
                    self.passed(args);
                    self.dynamic_args(args);
                    self.iface_methods(*iface, *slot)
                }
                hir::Callee::Intrinsic(
                    i @ (hir::Intrinsic::MutexWith | hir::Intrinsic::LockRead),
                ) => {
                    if let [m, cb] = args.as_slice() {
                        self.passed(std::slice::from_ref(m));
                        let method = match (i, self.cx.is_rwlock(m.ty)) {
                            (hir::Intrinsic::LockRead, _) => "read",
                            (_, true) => "write",
                            _ => "with",
                        };
                        self.withs.push((cb.clone(), cb.span, method));
                        match &cb.kind {
                            E::Closure(c) | E::FnRef(c, _) => self.callbacks.push(*c),
                            // Lowering takes the module lock for it: the code around it may
                            // not hold a `Mutex` then (locked_props.rs).
                            _ => self.facts.props = true,
                        }
                    }
                }
                hir::Callee::Intrinsic(i) => {
                    self.facts.resumes |= crate::replaced_holders::implicit::resumes(*i);
                    self.passed(args);
                    self.dynamic_args(args);
                }
            },
            _ => {}
        }
    }
}

/// The constants holding classes' values (`body::expr::ctor_values`, `ctor_objects`).
fn class_value_set(cx: &Ctx) -> FxHashSet<DefId> {
    cx.objects.class_values.iter().map(|(_, g)| *g).collect()
}
