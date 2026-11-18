//! Weak collections hidden from the thread checks by their written type
//! (docs/internals/design/weak-refs.md "Threads and `shared`"). A `WeakMap`, `WeakSet` or
//! `WeakRef` belongs to the task that made it, so a value holding one cannot cross threads:
//! as `with`'s result or something its callback uses from outside, as a `shared(...)` value, a
//! `spawn` argument, a channel message or an async closure's capture. The checks where those
//! are written (`body/expr/sync.rs`, `tasks.rs`, …) see the written type
//! (`Ctx::holds_weak`); this pass, after all bodies, finds the weak collections it hides:
//!
//! - **Interface and base-class values.** An interface value holds one when some class
//!   converted to the interface anywhere in the program (`ToDyn`) holds one; a class value,
//!   when some subclass upcast to it does (`Upcast`). The set of such interfaces and classes
//!   grows to a fixed point (a class can hold an interface value).
//! - **Type parameters.** A crossing whose type mentions a type parameter
//!   (`lockWith<T, R>(m, f): R { return m.with(f); }`) is checked at each instantiation: the
//!   generic crossings of each function are propagated to its callers with the call's type
//!   arguments (`crate::dispatch::Rounds`, as `crate::promise_copies` does for copies), and a
//!   call that makes one hold a weak collection is reported there. A conversion to an
//!   interface in generic code (`new H<T>(x)` as a `Peek`) is instantiated the same way.
//! - **Closures.** A closure literal passed to another task, or stored into a locked value
//!   (`ownership::locked`, `Ctx::weak_crossings`), is checked by what it captures.
//!
//! Plain interface use stays legal: only an interface that some weak-holding class is
//! converted to is treated as holding one.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::body::GenCopy;
use crate::ctx::Ctx;
use crate::dispatch::{instantiate, Dispatch, Rounds};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::inst_sites::carry_guard;
use crate::types::collect_params;

/// What a type found in a body does: crosses threads, or is converted to the interface or
/// base class (a source of what values of that type may hold).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Need {
    Cross(GenCopy, Span),
    Source(DefId),
}

/// What one body crosses and converts, and the closures it hands to another task or to `with`
/// or makes async ([`Found`]).
#[derive(Clone, Default)]
struct Scan {
    needs: Vec<(TyId, Need)>,
    task_fns: Vec<(DefId, Span)>,
    lock_fns: Vec<DefId>,
    async_fns: Vec<DefId>,
}

/// A finished unit's scans (`Ctx::weak_scans`, `crate::units`): what its bodies cross and
/// convert, and their facts for the instantiations ([`crate::promise_copies::collect`]). Its
/// bodies never change, so a later unit's check takes these in place of reading them again.
#[derive(Clone, Default)]
pub(crate) struct WeakScans {
    bodies: Vec<(DefId, Scan)>,
    facts: Vec<(DefId, crate::promise_copies::Facts)>,
}

/// The crossings and conversions of one body.
struct Found<'a, 'm> {
    cx: &'a Ctx<'m>,
    sends: &'a [DefId],
    std: bool,
    needs: Vec<(TyId, Need)>,
    /// Closure literals passed to another task, captures checked after the walk.
    task_fns: Vec<(DefId, Span)>,
    /// Closure literals passed to `with`, captures checked after the walk.
    lock_fns: Vec<DefId>,
    /// Async closures made here.
    async_fns: Vec<DefId>,
    /// Spans of `this` upcast for a base constructor (`super(...)`): not a use of the subclass
    /// as its base.
    super_receivers: FxHashSet<Span>,
}

impl crate::visit_ref::Visit for Found<'_, '_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::MutexWith | Intrinsic::LockRead),
                args,
            } => {
                let at = args.get(1).map_or(e.span, |f| f.span);
                self.needs
                    .push((e.ty, Need::Cross(GenCopy::LockResult, at)));
                if let Some(E::Closure(n)) = args.get(1).map(|f| &peel(f).kind) {
                    self.lock_fns.push(*n);
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::SharedNew),
                args,
            } => {
                if let [x] = args.as_slice() {
                    self.needs
                        .push((x.ty, Need::Cross(GenCopy::Shared, x.span)));
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn),
                args,
            } => {
                let mut calls = vec![];
                if let Some(p) = args.first() {
                    crate::body::expr::tasks::spawned_calls(p, &mut calls);
                }
                for a in calls.into_iter().flatten() {
                    self.needs.push((a.ty, Need::Cross(GenCopy::Task, a.span)));
                    if let E::Closure(n) = peel(a).kind {
                        self.task_fns.push((n, a.span));
                    }
                }
                // What the task's thread hands back: its result and the error it throws.
                if let TyKind::Promise(t, err) = self.cx.ty.kind(e.ty) {
                    let (t, err) = (*t, *err);
                    self.needs
                        .push((t, Need::Cross(GenCopy::TaskResult, e.span)));
                    self.needs
                        .push((err, Need::Cross(GenCopy::TaskError, e.span)));
                }
            }
            E::Call {
                callee: Callee::Def(m, _),
                args,
            } if self.sends.contains(m) => {
                if let Some(TyKind::Adt(_, targs)) = args.first().map(|r| self.cx.ty.kind(r.ty)) {
                    if let [t] = targs.as_slice() {
                        self.needs.push((*t, Need::Cross(GenCopy::Task, e.span)));
                    }
                }
                if let Some(E::Closure(n)) = args.get(1).map(|a| &peel(a).kind) {
                    self.task_fns.push((*n, e.span));
                }
            }
            E::Call {
                callee: Callee::Def(d, _),
                args,
            } if self
                .cx
                .try_fn(*d)
                .is_some_and(|f| f.kind == crate::defs::FnKind::Ctor) =>
            {
                if let Some(
                    recv @ Expr {
                        kind: E::Upcast(_), ..
                    },
                ) = args.first()
                {
                    self.super_receivers.insert(recv.span);
                }
            }
            E::Closure(n) if !self.std && self.cx.fn_info(*n).is_async => {
                self.async_fns.push(*n);
            }
            E::ToDyn { expr, impl_index } => {
                if let Some(imp) = self.cx.impls.get(*impl_index as usize) {
                    self.needs.push((expr.ty, Need::Source(imp.iface)));
                }
            }
            E::Upcast(inner) if !self.super_receivers.contains(&e.span) => {
                if let Some((base, _)) = self.cx.class_of(e.ty) {
                    if self.cx.class_of(inner.ty).is_some_and(|(d, _)| d != base) {
                        self.needs.push((inner.ty, Need::Source(base)));
                    }
                }
            }
            _ => {}
        }
    }
}

/// `e` without the conversions, shares and transfers around it.
fn peel(mut e: &Expr) -> &Expr {
    loop {
        e = match &e.kind {
            E::WrapSome(x) | E::Upcast(x) | E::ToDyn { expr: x, .. } => x,
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share | Intrinsic::Transfer),
                args,
            } if args.len() == 1 => &args[0],
            _ => return e,
        }
    }
}

/// The scan of each body: a finished unit's from its summary, the unit's own read here. std's
/// stage keeps its scans and facts for the app's (computed whether or not its own check needs
/// the facts: the app's may).
fn scans(cx: &mut Ctx, sends: &[DefId]) -> Vec<(DefId, Scan)> {
    use crate::units::Stage;
    let from = match cx.units_stage {
        Stage::App => cx.unit_base as usize,
        Stage::Both | Stage::Std => 0,
    };
    let mut out = match cx.units_stage {
        Stage::App => units_live_std(cx, &cx.unit_sums.weak_scans.bodies.clone()),
        Stage::Both | Stage::Std => vec![],
    };
    for (i, d) in cx.defs.iter().enumerate().skip(from) {
        let Some(Def::Fn(f)) = d else { continue };
        let def = DefId(i as u32);
        let std = cx.try_fn(def).is_some_and(|fi| cx.scopes[fi.module].is_std);
        let mut found = Found {
            cx: &*cx,
            sends,
            std,
            needs: vec![],
            task_fns: vec![],
            lock_fns: vec![],
            async_fns: vec![],
            super_receivers: FxHashSet::default(),
        };
        crate::visit_ref::block(&f.body.block, &mut found);
        let scan = Scan {
            needs: found.needs,
            task_fns: found.task_fns,
            lock_fns: found.lock_fns,
            async_fns: found.async_fns,
        };
        out.push((def, scan));
    }
    if cx.units_stage == Stage::Std {
        let facts = crate::promise_copies::collect(cx, 0);
        cx.unit_sums.weak_scans = WeakScans {
            bodies: out.clone(),
            facts,
        };
    }
    out
}

/// EXPERIMENT (#2943): of std's unit's `rows`, those of the std code lazy std would have had for
/// this program (`body::lazy_std::units_live_set`). The unit has every std module's bodies: a
/// conversion of a weak-holding class to an interface in a module the program never loads
/// would make that interface hold one in the program's checks (`std/task/signal.velt`'s
/// `Mutex<SignalObj>` refused in programs that don't use it). Code that isn't live never runs,
/// so it makes no such value.
fn units_live_std<T: Clone>(cx: &mut Ctx, rows: &[(DefId, T)]) -> Vec<(DefId, T)> {
    let live = crate::body::lazy_std::units_live_set(cx);
    (rows.iter())
        .filter(|(d, _)| live.contains(d))
        .cloned()
        .collect()
}

/// The facts of every body for the instantiations: a finished unit's from its summary.
fn facts(cx: &mut Ctx) -> Vec<(DefId, crate::promise_copies::Facts)> {
    match cx.units_stage {
        crate::units::Stage::App => {
            let mut out = units_live_std(cx, &cx.unit_sums.weak_scans.facts.clone());
            out.extend(crate::promise_copies::collect(cx, cx.unit_base as usize));
            out
        }
        crate::units::Stage::Std => cx.unit_sums.weak_scans.facts.clone(),
        crate::units::Stage::Both => crate::promise_copies::collect(cx, 0),
    }
}

/// The captures of closure `n`: type, name and where the captured variable is declared.
pub(crate) fn captures(cx: &Ctx, n: DefId) -> Vec<(TyId, String, Span)> {
    match &cx.defs[n.0 as usize] {
        Some(Def::Fn(f)) => f
            .captures
            .iter()
            .map(|c| &f.body.locals[c.inner.0 as usize])
            .map(|l| (l.ty, l.name.clone(), l.span))
            .collect(),
        _ => vec![],
    }
}

fn is_generic(cx: &Ctx, t: TyId) -> bool {
    let mut ps = vec![];
    collect_params(&cx.ty, t, &mut ps);
    !ps.is_empty()
}

/// Was a crossing of this kind with a written type holding a weak collection reported where
/// it is written? (Closures are checked here only.)
fn checked_where_written(what: &GenCopy) -> bool {
    !matches!(
        what,
        GenCopy::TaskFn(_) | GenCopy::LockFn(_) | GenCopy::TaskResult | GenCopy::TaskError
    )
}

/// `recorded`: the crossings `ownership::check_locked` found.
pub(crate) fn check(cx: &mut Ctx, recorded: crate::ownership::WeakCrossings) {
    // Each function's crossings and conversions.
    let sends = cx.channel_sends();
    let mut own: FxHashMap<DefId, Vec<(TyId, Need)>> = FxHashMap::default();
    for (d, t, what, at) in recorded {
        own.entry(d).or_default().push((t, Need::Cross(what, at)));
    }
    let (mut lock_fns, mut task_fns, mut async_fns) = (vec![], vec![], vec![]);
    for (def, scan) in scans(cx, &sends) {
        lock_fns.extend(scan.lock_fns);
        task_fns.extend(scan.task_fns);
        async_fns.extend(scan.async_fns);
        if !scan.needs.is_empty() {
            own.entry(def).or_default().extend(scan.needs);
        }
    }
    for n in lock_fns {
        for (t, name, at) in captures(cx, n) {
            let what = GenCopy::LockCapture(name);
            own.entry(n).or_default().push((t, Need::Cross(what, at)));
        }
    }
    for (n, at) in task_fns {
        for (t, name, _) in captures(cx, n) {
            let what = GenCopy::TaskFn(name);
            own.entry(n).or_default().push((t, Need::Cross(what, at)));
        }
    }
    for n in async_fns {
        for (t, name, at) in captures(cx, n) {
            let what = GenCopy::Capture(name);
            own.entry(n).or_default().push((t, Need::Cross(what, at)));
        }
    }
    let crosses = own
        .values()
        .flatten()
        .any(|(_, n)| matches!(n, Need::Cross(..)));
    if !crosses {
        return;
    }
    // Concrete crossings: (where, type, how, through the call of).
    let mut concrete: Vec<(Span, TyId, GenCopy, Option<DefId>)> = vec![];
    let mut sources: Vec<(DefId, TyId)> = vec![];
    let mut generic_cross = false;
    let mut generic_sources = vec![];
    for (t, n) in own.values().flatten() {
        match (n, is_generic(cx, *t)) {
            (Need::Cross(..), true) => generic_cross = true,
            (Need::Source(d), true) => generic_sources.push(*d),
            (Need::Cross(what, at), false) => concrete.push((*at, *t, what.clone(), None)),
            (Need::Source(d), false) => sources.push((*d, *t)),
        }
    }
    // Instantiating the generic code walks every body again: only when a generic crossing, or
    // a generic conversion to an interface or class a crossing value may hold, needs it.
    if generic_cross || {
        let reach = reachable(cx, &concrete, &sources);
        generic_sources.iter().any(|d| reach.contains(d))
    } {
        instantiations(cx, &own, &mut concrete, &mut sources);
    }
    // The interfaces and classes whose values may hold a weak collection, and a class that
    // makes each one.
    let mut hiding: FxHashSet<DefId> = FxHashSet::default();
    let mut why: FxHashMap<DefId, TyId> = FxHashMap::default();
    loop {
        let mut grew = false;
        for &(d, t) in &sources {
            if !hiding.contains(&d) && cx.holds_weak_with(t, &hiding) {
                hiding.insert(d);
                why.insert(d, t);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    concrete.sort_by_key(|(s, ..)| (s.file, s.lo, s.hi));
    let mut reported = FxHashSet::default();
    for (at, t, what, via) in concrete {
        let written = via.is_none() && checked_where_written(&what) && cx.holds_weak(t);
        if written || !cx.holds_weak_with(t, &hiding) || !reported.insert(at) {
            continue;
        }
        let mut d = crate::body::weak_copy_error(cx, t, &what, at);
        // The callback of a generic helper's `with` written in this call is reported itself
        // (`ownership::locked`): the same error once is enough.
        let inside = |s: Span| s.file == at.file && s.lo >= at.lo && s.hi <= at.hi;
        let again = via.is_some()
            && cx.diags.iter().any(|x| {
                x.is_error()
                    && x.message == d.message
                    && x.labels.first().is_some_and(|l| inside(l.span))
            });
        if again {
            continue;
        }
        if let Some(g) = via {
            let name = cx.fn_info(g).name.clone();
            let name = name.rsplit("::").next().unwrap_or(&name).to_string();
            d = d.with_note(format!(
                "`{name}` does this with a value of a type parameter, which this call makes `{}`",
                cx.display(t)
            ));
        }
        if let Some(h) = hidden(cx, t, &hiding, &mut FxHashSet::default()) {
            if let Some(src) = why.get(&h).copied() {
                let (hn, kind) = match (cx.iface(h), cx.adt(h)) {
                    (Some(i), _) => (i.name.clone(), "an interface value"),
                    (_, Some(a)) => (a.name.clone(), "a class value"),
                    _ => (String::new(), "a value"),
                };
                let sn = cx.display(src);
                d = d.with_note(format!(
                    "a `{hn}` may be a `{sn}`, which holds a weak collection: {kind} may hold anything converted to it in the program"
                ));
            }
        }
        cx.error(d);
    }
}

/// The interfaces and classes a value crossing threads (`concrete`) may hold, following what
/// is converted to them (`sources`).
fn reachable(
    cx: &mut Ctx,
    concrete: &[(Span, TyId, GenCopy, Option<DefId>)],
    sources: &[(DefId, TyId)],
) -> FxHashSet<DefId> {
    let mut by_target: FxHashMap<DefId, Vec<TyId>> = FxHashMap::default();
    for &(d, t) in sources {
        by_target.entry(d).or_default().push(t);
    }
    let mut out = FxHashSet::default();
    let mut seen = FxHashSet::default();
    let mut todo: Vec<TyId> = concrete.iter().map(|c| c.1).collect();
    while let Some(t) = todo.pop() {
        if !seen.insert(t) {
            continue;
        }
        let kind = cx.ty.kind(t).clone();
        if let TyKind::Adt(d, _) | TyKind::Dyn(d, _) = kind {
            if out.insert(d) {
                todo.extend(by_target.get(&d).into_iter().flatten().copied());
            }
        }
        match kind {
            TyKind::Adt(d, args) => {
                let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                    crate::defs::DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    crate::defs::DefInfo::Enum(e) => e
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.iter().copied())
                        .collect(),
                    _ => vec![],
                };
                todo.extend(tys.into_iter().map(|f| cx.subst(f, &args)));
            }
            k => todo.extend(crate::types::children(&k)),
        }
    }
    out
}

/// The interface or class in `t` that hides a weak collection, if `t` holds one only that way.
fn hidden(
    cx: &mut Ctx,
    t: TyId,
    hiding: &FxHashSet<DefId>,
    seen: &mut FxHashSet<TyId>,
) -> Option<DefId> {
    if !seen.insert(t) || cx.holds_weak(t) {
        return None;
    }
    let parts: Vec<TyId> = match cx.ty.kind(t).clone() {
        TyKind::Adt(d, _) | TyKind::Dyn(d, _) if hiding.contains(&d) => return Some(d),
        TyKind::Shared(_) | TyKind::FnPtr { .. } => return None,
        TyKind::Adt(d, args) => {
            let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                crate::defs::DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                crate::defs::DefInfo::Enum(e) => e
                    .variants
                    .iter()
                    .flat_map(|v| v.payload.iter().copied())
                    .collect(),
                _ => vec![],
            };
            tys.into_iter().map(|f| cx.subst(f, &args)).collect()
        }
        k => crate::types::children(&k),
    };
    parts.into_iter().find_map(|p| hidden(cx, p, hiding, seen))
}

/// A type to check, with the call that instantiated it.
type Req = ((TyId, Need), Option<(Span, DefId)>);

/// The generic crossings and conversions of `own`, instantiated at every call (module docs):
/// the concrete ones are added to `concrete` (with the call) and `sources`.
fn instantiations(
    cx: &mut Ctx,
    own: &FxHashMap<DefId, Vec<(TyId, Need)>>,
    concrete: &mut Vec<(Span, TyId, GenCopy, Option<DefId>)>,
    sources: &mut Vec<(DefId, TyId)>,
) {
    let mut facts = facts(cx);
    let mut dispatch = Dispatch::default();
    for (_, fx) in &mut facts {
        for (t, span) in std::mem::take(&mut fx.dispatch) {
            for (m, args) in dispatch.targets(cx, t) {
                fx.dyn_calls.push((m, args, span));
            }
        }
    }
    let reads = facts.iter().enumerate().flat_map(|(i, (_, fx))| {
        let calls = fx.calls.iter().chain(&fx.dyn_calls).map(|(d, _, _)| *d);
        calls
            .chain(fx.closures.iter().copied())
            .map(move |d| (i, d))
    });
    let mut rounds = Rounds::new(facts.len(), reads);
    let mut needs: FxHashMap<DefId, Vec<(TyId, Need, u64)>> = FxHashMap::default();
    let mut found_at: FxHashSet<(Span, TyId, Need)> = FxHashSet::default();
    while let Some(i) = rounds.pop() {
        let (f, fx) = &facts[i];
        // With guard bits (`inst_sites::carry_guard`).
        let mut reqs: Vec<(Req, u64)> = own
            .get(f)
            .into_iter()
            .flatten()
            .map(|n| ((n.clone(), None), 0))
            .collect();
        for ((d, targs, span), site) in fx.calls.iter().zip(&fx.guards) {
            for (t, n, need) in needs.get(d).into_iter().flatten() {
                if let Some(g) = carry_guard(cx, *need, targs, *site) {
                    reqs.push((((cx.subst(*t, targs), n.clone()), Some((*span, *d))), g));
                }
            }
        }
        for (m, args, span) in &fx.dyn_calls {
            for (t, n, need) in needs.get(m).into_iter().flatten() {
                let Some(g) = carry_guard(cx, *need, args, 0) else {
                    continue;
                };
                if let Some(t) = instantiate(cx, *t, args) {
                    reqs.push((((t, n.clone()), Some((*span, *m))), g));
                }
            }
        }
        for c in &fx.closures {
            for (t, n, need) in needs.get(c).into_iter().flatten() {
                reqs.push((((*t, n.clone()), None), *need));
            }
        }
        let mut grew = false;
        for (((t, n), at), guard) in reqs {
            if is_generic(cx, t) {
                let ns = needs.entry(*f).or_default();
                if !ns.contains(&(t, n.clone(), guard)) {
                    ns.push((t, n, guard));
                    grew = true;
                }
            } else if let Some((span, callee)) = at {
                if !found_at.insert((span, t, n.clone())) {
                    continue;
                }
                match n {
                    Need::Cross(what, _) => concrete.push((span, t, what, Some(callee))),
                    Need::Source(d) => sources.push((d, t)),
                }
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    WeakScans {
        bodies, facts,
    }
    Scan {
        needs, task_fns, lock_fns, async_fns,
    }
}
crate::units::codec::enums! {
    Need {
        0 => Cross(f0, f1),
        1 => Source(f0),
    }
}
