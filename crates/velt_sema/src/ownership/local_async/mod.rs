//! Local async closures (#208, https://veltlang.com/docs/reference/functions/ "Captures"): an async closure that
//! never reaches a thread boundary captures like a sync closure, so it may modify what it
//! captured and the enclosing code may assign the variables it captured, as in JavaScript.
//!
//! Every call of an async closure is a started promise on the caller's task, which never runs
//! at the same time as the task's other code. What makes sharing unsafe is another thread: a
//! spawned task, a `shared(...)` value, a channel, or a
//! promise settled for another task. An async closure is **non-local** when its value may
//! reach one of them ([`graph`]):
//! - spawned: a spawned closure literal (`spawn(async () => …)`), the callee of a spawned call
//!   (`spawn(f())`), an argument of one (`spawn(run(f))`) or what the spawned function returns;
//! - the argument of `shared(...)` or `new Mutex(...)`, a channel send, a value settling a
//!   promise (`Intrinsic::Transfer`);
//! - through locals, parameters (from every direct caller), return values, and the captures of
//!   a closure that is itself non-local;
//! - through the heap, by type ([`types`]): a closure stored in a field, element or map value,
//!   or passed to an unknown callee (a function value, an interface method or an overridden
//!   method, which may keep it), is non-local when a value crossing a boundary has a type
//!   reaching a function type of the same shape;
//! - an argument of a call through a function value that may be a closure whose parameter
//!   crosses (`resolve(x)`), matched by type in each instantiation of the closure's function.
//!
//! An unknown callee is not a boundary itself (#2726): its call runs on this task. A callback it
//! keeps and later runs on another thread gets there through a boundary (a spawn, a channel, a
//! settled promise, `shared(...)`), whose transfer copies what is still shared (the task-creation
//! rule, docs/internals/design/semantics-stage2.md §6). A value of unknown origin keeps the type it had where it flowed in, so a
//!   generic parameter that crosses brings its callers' concrete types. A boundary inside a
//!   library (std, a package) reaches the program's closures only through values the program
//!   hands to the library ([`heap_reaches`]): the types of what the library itself makes and
//!   sends to another thread stand for the library's own closures.
//!
//! A local async closure (`FnDef::shares_captures`) shares its by-value captures with each call
//! (lowering's `take_capture`) and its assigned captured variables live in cells
//! (`crate::moves`, like a generator closure's). A non-local one keeps today's rules: each call
//! copies what it captured, and modifying a capture is an error naming the boundary.
//!
//! The run time covers paths the analysis does not follow: a closure environment, and a
//! captured variable's cell, that are still shared when they cross are copied (velt_vir
//! glue/transfer_env.rs), and a local closure that becomes callable from several threads at
//! once panics in the many-threads check (glue/many.rs).

mod call_sites;
mod callbacks;
mod changes;
mod fed;
mod graph;
mod keyed;
mod reach;
mod record;
mod targets;
mod types;
mod unknown_callees;
mod values;
mod views;

use rustc_hash::FxHashMap;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, TyId, TyKind};

use graph::{Boundary, Graph, Node, Why, CROSSES, STORED};

pub(crate) use changes::check_crossing_changes;
pub(crate) use record::record_changed_captures;

/// A callback that a library runs on other threads: the closure, and where (with the label) the
/// program hands it to the library.
#[derive(Clone)]
pub(crate) struct Crossing {
    pub closure: DefId,
    pub at: velt_common::Span,
    pub label: String,
}

/// Decide which async closures are local, mark them (`FnDef::shares_captures`), and report the
/// non-local ones that modify a captured variable.
pub(crate) fn infer_local_async(cx: &mut Ctx) {
    cx.crossing_callbacks.clear();
    cx.in_shared.clear();
    let g = graph::build(cx);
    let mut p = Propagation::new(&g);
    for &(n, f, why) in &g.seeds {
        p.set(n, f, why);
    }
    let mut seen = FxHashMap::default();
    let mut fns: FxHashMap<TyId, Why> = FxHashMap::default();
    let mut rooted = 0;
    let mut resolved = 0;
    // The types a stored closure literal's type has in its function's instantiations, by the
    // function and the type.
    let mut lit_insts: FxHashMap<(DefId, TyId), Vec<TyId>> = FxHashMap::default();
    loop {
        p.run(cx, &g);
        resolve_hits(cx, &g, &mut p, &mut resolved);
        types::crossing_fns(cx, &p.roots[rooted..], &mut seen, &mut fns);
        rooted = p.roots.len();
        let mut changed = false;
        for (n, node) in g.nodes.iter().enumerate() {
            let Node::Lit(c) = *node else { continue };
            if p.flags[n] & CROSSES != 0 || p.flags[n] & STORED == 0 {
                continue;
            }
            // A callback wrapper of an async closure (`body/expr/callback.rs`) is matched by the
            // async closure's type, or by its own type only where nothing generic stands for it:
            // `input.onChange = async (v) => …` in a `(v: string) => void` field is not the
            // prelude's `resolve`, a `(T) => void`, but is any `(v: string) => void` that crosses.
            let wrapped = cx.callback_wrappers.get(&c).copied();
            // A sync closure is matched the same strict way: `resolve`'s `(T) => void` stands for
            // no sync callback the user stores.
            let sync = matches!(&cx.defs[c.0 as usize], Some(Def::Fn(f)) if !f.is_async);
            // A closure of a generic function's type (std's `uncaughtGet`, a `() => V`) is what
            // that type is in the function's instantiations (`() => number` from a getter), which
            // a `() => void` can't hold; one still generic matches anything, as before.
            let lit = g.tys[n];
            // The crossing types the literal's own type matches. One of its instantiations can
            // match only these: a type parameter matches anything.
            let mut found: Vec<(TyId, Why)> = (fns.iter())
                .filter(|(t, w)| heap_reaches(cx, w, c) && lit_matches(cx, wrapped, sync, lit, **t))
                .map(|(t, w)| (*t, *w))
                .collect();
            // A literal of a generic function's type (std's `uncaughtGet`, a `() => V`) is what
            // that type is in the function's instantiations (`() => number` from a getter), which a
            // `() => void` can't hold. Looked up once per function and type, and only when its own
            // type matched something. A library function no call instantiates (a method reached
            // only through an interface) gives none, and its generic type matches as before.
            if !found.is_empty() && types::mentions_param(cx, lit) {
                let key = (outermost(&g, c), lit);
                if let std::collections::hash_map::Entry::Vacant(e) = lit_insts.entry(key) {
                    let mut insts = instances(cx, &g, c, lit);
                    if insts.is_empty() {
                        insts.push(lit);
                    }
                    e.insert(insts);
                }
                let lits = &lit_insts[&key];
                found.retain(|(t, _)| lits.iter().any(|l| lit_matches(cx, wrapped, sync, *l, *t)));
            }
            // Several crossing types may match: report one in user code, the earliest.
            // A crossing parameter of a generic type (`Graph::wild`) may carry any closure.
            let wild = p.wild.iter().filter(|w| heap_reaches(cx, w, c)).copied();
            let why = (found.into_iter().map(|(_, w)| w))
                .chain(wild)
                .min_by_key(|w| w.rank(cx));
            if let Some(w) = why {
                p.set(n, CROSSES, Some(w));
                changed = true;
            }
        }
        changed |= call_sites::crossing_params(cx, &g, &mut p);
        if !changed {
            break;
        }
    }
    let reported = callbacks::check_crossing_callbacks(cx, &g, &p.flags);
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Lit(c) = *node else { continue };
        let why = p.why[n];
        let local = p.flags[n] & CROSSES == 0;
        finish(cx, c, local, why);
        // A standard-library function that runs it on other threads: spawns it, from the user's
        // call (`serve`'s handler); a channel or a promise hands a value over once.
        let to_std = (p.why[n])
            .filter(|w| p.flags[n] & CROSSES != 0 && w.in_library && w.boundary == Boundary::Spawn);
        if let (Some(w), false) = (to_std, reported.contains(&c)) {
            crossing_sync(cx, c, Some(w));
        }
        // Followed to `shared(...)` by flow: every thread holding it calls it, each call on a copy of
        // what it captured, so a change would be lost. A match by function type alone isn't enough
        // to refuse a closure that may never get there: the many-threads check's strict walk panics
        // on the exact closure if it does. A `Mutex` around it runs the calls one at a time.
        let in_shared = p.shared[n].filter(|_| p.flags[n] & CROSSES != 0);
        if let Some(w) = in_shared {
            cx.in_shared.push(c);
            crossing_sync(cx, c, Some(w));
            if !cx.callback_wrappers.contains_key(&c)
                && !cx.try_fn(c).is_some_and(|i| cx.scopes[i.module].is_library)
            {
                let label = reaches(cx, &w);
                cx.crossing_callbacks.push(Crossing {
                    closure: c,
                    at: w.span,
                    label,
                });
            }
        }
        if let Some(w) = to_std {
            let copied = crate::ownership::Copied::Crossing(w.span, reaches(cx, &w));
            crate::ownership::check_copied_closure(cx, c, &copied);
            // Changes through calls are known once mutation inference ran (`changes`).
            // A callback wrapper the compiler made around an async closure (`body/expr/callback.rs`)
            // is checked through that closure.
            let wrapper = cx.callback_wrappers.contains_key(&c);
            if !wrapper && !cx.try_fn(c).is_some_and(|i| cx.scopes[i.module].is_library) {
                let label = reaches(cx, &w);
                cx.crossing_callbacks.push(Crossing {
                    closure: c,
                    at: w.span,
                    label,
                });
            }
        }
    }
}

/// May closure literal `l` (its type, or one of its instantiations) be a value of crossing function
/// type `t`? A callback wrapper of an async closure (`body/expr/callback.rs`) is matched by the
/// async closure's type `wrapped`, or by its own only where nothing generic stands for it; a sync
/// closure the same strict way (`resolve`'s `(T) => void` stands for no sync callback the user
/// stores).
fn lit_matches(cx: &Ctx, wrapped: Option<TyId>, sync: bool, l: TyId, t: TyId) -> bool {
    match wrapped {
        Some(inner) => {
            types::may_be(cx, inner, t)
                || (!types::mentions_param(cx, t) && types::may_be(cx, l, t))
        }
        None if sync => !types::mentions_param(cx, t) && types::may_be(cx, l, t),
        None => types::may_be(cx, l, t),
    }
}

/// May the crossing types of `w` (the function types a value crossing there holds) stand for
/// closure literal `c` stored in the heap? A library's closures, yes. The program's closures only
/// when `w` is in the program: a boundary inside a library (`w.library`) reaches what the program
/// hands to the library through the graph, as a value passed at the program's call into it
/// ([`Why::step`]), and the types of what the library makes there itself only hold the
/// library's own closures. Matching those by shape alone would make every program closure stored
/// in a `(() => void) | null` cross once a library spawns anything holding such a field (fetch's
/// HTTP/2 session and its `onClose`).
fn heap_reaches(cx: &Ctx, w: &Why, c: DefId) -> bool {
    !w.library || cx.try_fn(c).is_some_and(|i| cx.scopes[i.module].is_library)
}

/// The crossing values of a generic type found since `done` (`Graph::wild`, `Graph::generic_roots`):
/// each carries
/// what its type is in the instantiations of its function, a crossing type for the heap rule
/// (a closure in `Job[]` or `Box<Job>` too). The type in an instantiation that is unknown (no
/// call gives the type arguments) stays generic, and the parameter may carry any closure.
fn resolve_hits(cx: &mut Ctx, g: &Graph, p: &mut Propagation, done: &mut usize) {
    while let Some(&(f, ty, w)) = p.hits.get(*done) {
        *done += 1;
        // A method a dispatched call may reach has instantiations no direct call shows (a
        // `Runner<FnTask>` holding an `Sp<FnTask>`). Its class's type parameters are what every
        // `new` of the class, and every subclass's `extends`, gives them ([`class_insts`]); its own
        // type parameters (`run<U>`), which a dispatched call may give anything, and a class whose
        // objects come from nowhere the graph shows, let the value carry any closure.
        let owner = outermost(g, f);
        if g.dispatched.binary_search_by_key(&owner.0, |d| d.0).is_ok() {
            let insts = match method_class(cx, owner) {
                Some((_, own)) if types::mentions_param_from(cx, ty, own) => None,
                Some((class, _)) => class_insts(cx, g, class, 0),
                None => None,
            };
            match insts {
                Some(sites) => {
                    for (caller, targs) in sites {
                        let t = cx.subst(ty, &targs);
                        for t in instances(cx, g, caller, t) {
                            push_instance(cx, p, t, w);
                        }
                    }
                    continue;
                }
                None => p.wild.push(w),
            }
        }
        for t in instances(cx, g, f, ty) {
            push_instance(cx, p, t, w);
        }
    }
}

/// What a crossing value of type `t` (an instantiation of a generic one) carries: `t` itself,
/// for the heap rule, or any closure when `t` is still generic.
fn push_instance(cx: &Ctx, p: &mut Propagation, t: TyId, w: Why) {
    if types::mentions_param(cx, t) {
        // Its type, a type parameter, says nothing to the reader: the label names the boundary.
        p.wild.push(w);
    } else {
        p.roots.push((t, w));
    }
}

/// The class of method `m`, with the number of its type parameters (the method's own come after).
fn method_class(cx: &Ctx, m: DefId) -> Option<(DefId, u32)> {
    let Some(Def::Fn(f)) = &cx.defs[m.0 as usize] else {
        return None;
    };
    match cx.ty.kind(f.self_ty?) {
        TyKind::Adt(class, args) => Some((*class, args.len() as u32)),
        _ => None,
    }
}

/// Every instantiation of `class`, with the function it is made in: the type arguments of each
/// `new` (or literal) of it, and of each subclass's `extends` (`class NumQueue extends
/// AsyncQueue<number>`), followed through the subclass's own instantiations where that names the
/// subclass's type parameters. `None` where none is seen (its objects come from somewhere the graph
/// doesn't show) or the chain is too deep.
fn class_insts(
    cx: &mut Ctx,
    g: &Graph,
    class: DefId,
    depth: u32,
) -> Option<Vec<(DefId, Vec<TyId>)>> {
    if depth > 8 {
        return None;
    }
    let mut out = g.news.get(&class).cloned().unwrap_or_default();
    for (sub, base) in types::direct_subclasses(cx, class) {
        let TyKind::Adt(_, bargs) = cx.ty.kind(base).clone() else {
            continue;
        };
        if bargs.iter().any(|t| types::mentions_param(cx, *t)) {
            for (caller, sargs) in class_insts(cx, g, sub, depth + 1)? {
                let args = bargs.iter().map(|t| cx.subst(*t, &sargs)).collect();
                out.push((caller, args));
            }
        } else {
            out.push((sub, bargs));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The function that closure `c` is written in, outside every closure (`c` itself if it is no
/// closure).
fn outermost(g: &Graph, mut c: DefId) -> DefId {
    let mut hops = 0;
    while let (Some(&p), true) = (g.parent.get(&c), hops < 64) {
        c = p;
        hops += 1;
    }
    c
}

/// The types closure literal `c` (of type `ty`) has in the instantiations of the generic
/// function that creates it: `(value: T) => void` in `withResolvers<T>` is `(value: Job) =>
/// void` where the program calls `withResolvers<Job>()`. Generic parameters of callers that
/// are generic themselves are followed to their own callers; where that gives no answer (or too
/// many), the generic type itself, whose parameters match anything; a generic library function
/// the program never calls gives none.
fn instances(cx: &mut Ctx, g: &Graph, c: DefId, ty: TyId) -> Vec<TyId> {
    let mut owner = c;
    let mut hops = 0;
    while let (Some(&p), true) = (g.parent.get(&owner), hops < 64) {
        owner = p;
        hops += 1;
    }
    let mut out = vec![];
    let mut work = vec![(owner, ty, 0)];
    while let Some((f, t, depth)) = work.pop() {
        let mut params = vec![];
        crate::types::collect_params(&cx.ty, t, &mut params);
        if params.is_empty() {
            out.push(t);
            continue;
        }
        let sites = g.insts.get(&f).cloned().unwrap_or_default();
        // A generic library function that the program never calls directly
        // makes no such closure (its methods are called with their type arguments too).
        let library = cx.try_fn(f).is_some_and(|i| cx.scopes[i.module].is_library);
        if sites.is_empty() && library {
            continue;
        }
        if sites.is_empty() || depth > 4 || out.len() + work.len() > 64 {
            out.push(t);
            continue;
        }
        for (caller, targs) in sites {
            let mut caller_owner = caller;
            let mut hops = 0;
            while let (Some(&p), true) = (g.parent.get(&caller_owner), hops < 64) {
                caller_owner = p;
                hops += 1;
            }
            let s = cx.subst(t, &targs);
            work.push((caller_owner, s, depth + 1));
        }
    }
    out.sort_by_key(|t| t.0);
    out.dedup();
    out
}

/// A sync closure handed to a standard-library function that runs it on other threads (a
/// server's handler: a copy on each connection's task) would change only those copies of what it
/// captured, out of sight, so modifying a capture is an error there, as for an async closure. A
/// closure the program hands to `spawn` itself is copied for the task with what it captures, as
/// https://veltlang.com/docs/reference/async/ says, and may change its copy. A library's closures (std's, a
/// package's) are not the program's to change.
fn crossing_sync(cx: &mut Ctx, c: DefId, why: Option<Why>) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    if f.is_async || f.is_generator || cx.try_fn(c).is_some_and(|i| cx.scopes[i.module].is_library)
    {
        return;
    }
    count_through(cx, c, why);
    report_mutated(cx, c, why, "closure");
}

/// Counts what closure `c` changes through the closures it captured with what it changes itself,
/// at a boundary in the program. What a library runs on other threads has its own check, which
/// names the call (`callbacks`).
fn count_through(cx: &mut Ctx, c: DefId, why: Option<Why>) {
    if why.is_some_and(|w| !w.in_library) {
        let info = cx.fn_info_mut(c);
        let through = std::mem::take(&mut info.mutated_through);
        info.mutated_captures.extend(through);
    }
}

/// Record what was decided for async closure `c`.
fn finish(cx: &mut Ctx, c: DefId, local: bool, why: Option<Why>) {
    let Some(Def::Fn(f)) = &mut cx.defs[c.0 as usize] else {
        return;
    };
    if !f.is_async || f.is_generator {
        // A plain closure whose promise leaves its task (`const go = () => work(c)` with
        // `spawn(go())`): the promise holds what the closure passed on from its captures.
        let (generator, ret) = (f.is_generator, f.ret);
        if !local && !generator && matches!(cx.ty.kind(ret), TyKind::Promise(..)) {
            report_atomics(cx, c, why, true);
        }
        return;
    }
    if local {
        f.shares_captures = true;
        return;
    }
    count_through(cx, c, why);
    report_mutated(cx, c, why, "async closure");
    report_generators(cx, c, why);
    report_atomics(cx, c, why, false);
}

/// A non-local async closure copies what it captured for each call (and a plain closure's
/// promise carries what the closure passed on), so a captured value holding an `Atomic` (outside
/// a `shared`) would be changed in a copy the task that created it never sees: report it. A closure literal spawned where it is written is reported by the boundary
/// check instead, which names the variable used again (`boundary.rs`).
fn report_atomics(cx: &mut Ctx, c: DefId, why: Option<Why>, plain: bool) {
    let closure = cx.def_spans.get(c.0 as usize).copied();
    if let (Some(at), Some(w)) = (closure, why) {
        if w.boundary == Boundary::Spawn
            && at.file == w.span.file
            && w.span.lo <= at.lo
            && at.hi <= w.span.hi
        {
            return;
        }
    }
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let caps: Vec<(TyId, String, velt_common::Span)> = f
        .captures
        .iter()
        .map(|cap| &f.body.locals[cap.inner.0 as usize])
        .map(|l| (l.ty, l.name.clone(), l.span))
        .collect();
    for (ty, name, at) in caps {
        if cx.atomic_value(ty).is_some() || !cx.holds_atomic(ty) {
            continue;
        }
        let tn = cx.display(ty);
        let mut d = Diagnostic::error(
            match plain {
                true => format!("a closure whose promise may run on another thread captures `{name}`, whose `{tn}` holds an `Atomic`"),
                false => format!("an async closure that may run on another thread captures `{name}`, whose `{tn}` holds an `Atomic`"),
            },
            at,
        );
        if let Some(w) = why {
            let label = reaches(cx, &w);
            d = d.with_label(w.span, label);
        }
        cx.error(d.with_note(format!(
            "{}, and changes to the copy's atomics would not reach `{name}`: share it so every task changes the same atomics: `const {name} = shared(new {tn}(…))`",
            match plain {
                true => "the promise it returns goes to the other task with a copy of what the closure passed on",
                false => "such a closure copies what it captured for each call",
            }
        )));
    }
}

/// A non-local async closure copies what it captured for each call, and a generator's state
/// cannot be copied: report each captured generator (a local one shares it, as in JS).
fn report_generators(cx: &mut Ctx, c: DefId, why: Option<Why>) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let caps: Vec<(TyId, String, velt_common::Span)> = f
        .captures
        .iter()
        .map(|cap| &f.body.locals[cap.inner.0 as usize])
        .map(|l| (l.ty, l.name.clone(), l.span))
        .collect();
    let gens: Vec<_> = caps
        .into_iter()
        .filter(|c| cx.holds_generator(c.0))
        .collect();
    for (_, name, at) in gens {
        let mut d = Diagnostic::error(
            format!("an async closure that may run on another thread cannot capture the generator `{name}`"),
            at,
        );
        if let Some(w) = why {
            let label = reaches(cx, &w);
            d = d.with_label(w.span, label);
        }
        cx.error(d.with_note("such a closure copies what it captured for each call, and a generator's suspended state cannot be copied: create the generator inside the closure, or keep the closure on the task that created it"));
    }
}

/// The label naming where a non-local closure leaves its task.
pub(in crate::ownership::local_async) fn reaches(cx: &Ctx, w: &Why) -> String {
    let place = match w.boundary {
        // The `spawn` is in a library: the program's call hands the value to it.
        Boundary::Spawn if w.in_library => {
            "is passed here to a function that runs it on other threads"
        }
        Boundary::Spawn => "reaches `spawn` here",
        Boundary::Shared => "reaches `shared(...)` here",
        Boundary::Mutex => "is put in a `Mutex` here, which other threads may lock",
        Boundary::Channel => "is sent on a channel here",
        Boundary::Settle => "settles a promise here, which another task may await",
    };
    match w.via {
        Some(t) => {
            let shown = cx.display(t);
            let article = match shown.chars().next() {
                Some(c) if "AEIOUaeiou".contains(c) => "an",
                _ => "a",
            };
            format!("but it is stored in {article} `{shown}`, which {place}")
        }
        None => format!("but it {place}"),
    }
}

/// In [`Propagation::work`]: a node that crosses already reaches `shared(...)` too.
const SHARED_WALK: u8 = 8;

/// Flags of every node, with why `CROSSES` was set, propagated from a
/// node to the nodes its values come from.
struct Propagation {
    flags: Vec<u8>,
    why: Vec<Option<Why>>,
    /// How a crossing node is followed by flow to a function in `shared(...)`, if it is, whichever
    /// boundary came first: a sync closure may change its copy when spawned, and not there.
    shared: Vec<Option<Why>>,
    work: Vec<(usize, u8)>,
    roots: Vec<(TyId, Why)>,
    /// Why a crossing local of a generic type, with no known instantiation, crosses: it may carry
    /// any closure (`Graph::wild`).
    wild: Vec<Why>,
    /// Crossing values of a generic type (`Graph::wild` locals, `Graph::generic_roots`): the
    /// function, the type and why, to resolve through the function's instantiations
    /// ([`resolve_hits`]).
    hits: Vec<(DefId, TyId, Why)>,
}

impl Propagation {
    fn new(g: &Graph) -> Self {
        Propagation {
            flags: vec![0; g.nodes.len()],
            why: vec![None; g.nodes.len()],
            shared: vec![None; g.nodes.len()],
            work: vec![],
            roots: g.roots.clone(),
            wild: vec![],
            hits: g.generic_roots.clone(),
        }
    }

    fn set(&mut self, n: usize, f: u8, why: Option<Why>) {
        if let (CROSSES, Some(w), None) = (f, why, self.shared[n]) {
            if w.boundary == Boundary::Shared && w.via.is_none() && !w.in_value {
                self.shared[n] = Some(w);
                if self.flags[n] & CROSSES != 0 {
                    self.work.push((n, SHARED_WALK));
                }
            }
        }
        if self.flags[n] & f != 0 {
            // A path fewer calls deep inside a library replaces a deeper one: only a path at the
            // library's own level may leave it for the program (`Why::step`).
            if f != CROSSES {
                return;
            }
            if let (Some(old), Some(new)) = (self.why[n], why) {
                if old.library && new.library && new.depth < old.depth {
                    self.why[n] = Some(new);
                    self.work.push((n, f));
                }
            }
            return;
        }
        self.flags[n] |= f;
        if f == CROSSES {
            self.why[n] = why;
        }
        self.work.push((n, f));
    }

    fn run(&mut self, cx: &Ctx, g: &Graph) {
        while let Some((n, f)) = self.work.pop() {
            let why = match f {
                CROSSES => self.why[n],
                SHARED_WALK => self.shared[n],
                _ => None,
            };
            // The walk for `shared(...)` goes where a crossing goes, with that reason.
            let f = match f {
                SHARED_WALK => CROSSES,
                f => f,
            };
            if let Some(w) = why {
                self.root(cx, g, n, f, w);
            }
            for &(m, site) in &g.srcs[n] {
                match why {
                    Some(w) => {
                        if let Some(w) = w.step(matches!(g.nodes[m], Node::Ret(_)), site) {
                            self.set(m, f, Some(w));
                        }
                    }
                    None => self.set(m, f, None),
                }
            }
            // A closure that leaves (or may) takes what it captured with it.
            if f != STORED {
                if let Some(caps) = g.captures.get(&n) {
                    for &i in caps {
                        self.set(i, f, why);
                    }
                }
            }
        }
    }

    /// A crossing node's value takes what it reaches with it: its type is a root of the
    /// type-based part. A function value of known origin is followed by the graph instead.
    fn root(&mut self, cx: &Ctx, g: &Graph, n: usize, f: u8, w: Why) {
        let ty = g.tys[n];
        let rooted = f == CROSSES && (g.unknown[n] || !types::fn_like(cx, ty));
        // Only a value of unknown origin (a read, or what one fed): one the graph follows, such as
        // a parameter of a function called by flow, crosses where its values come from instead.
        if let (true, Node::Local(d, _) | Node::Ret(d)) = (
            rooted && g.unknown[n] && g.wild.binary_search(&n).is_ok(),
            g.nodes[n],
        ) {
            self.hits.push((d, ty, w));
        }
        if rooted && !matches!(g.nodes[n], Node::Lit(_)) {
            self.roots.push((ty, w));
        }
        // What flowed in from places the graph does not follow, with the types it had there:
        // a generic parameter's node gets its callers' concrete types.
        for &(t, site) in &g.inflows[n] {
            if let (true, Some(w)) = (f == CROSSES && t != ty, w.step(false, site)) {
                self.roots.push((t, w));
            }
        }
    }
}

/// "this `what` modifies captured `x`" for each capture closure `c` modifies, naming the
/// boundary `why` it reaches.
fn report_mutated(cx: &mut Ctx, c: DefId, why: Option<Why>, what: &str) {
    let mutated = std::mem::take(&mut cx.fn_info_mut(c).mutated_captures);
    let closure = cx.def_spans.get(c.0 as usize).copied();
    for (name, at) in mutated {
        // Also through the closures that call `c` (`mutated_captures`): one change, one error.
        if cx.reported_mutations.contains(&(name.clone(), at)) {
            continue;
        }
        cx.reported_mutations.push((name.clone(), at));
        // The capture is then moved into the closure: no second error for a later use of it.
        if let Some(span) = closure {
            cx.reported_captures.push((name.clone(), span));
        }
        let mut d = Diagnostic::error(
            format!("this {what} modifies captured `{name}`, so it must stay on the task that created it"),
            at,
        );
        if let Some(w) = why {
            let label = reaches(cx, &w);
            d = d.with_label(w.span, label);
        }
        let runs = match why.map(|w| w.boundary) {
            Some(Boundary::Spawn) => "the spawned task runs it on another thread",
            Some(Boundary::Shared | Boundary::Mutex) => {
                "every thread holding the `shared` value may call it"
            }
            Some(Boundary::Channel) => "the task receiving it runs it on another thread",
            Some(Boundary::Settle) => "the task awaiting the promise may run it on another thread",
            None => "another thread may run it",
        };
        // `shared` holds a number (`add`) or any value (`get`/`set`); a collection or an object
        // is changed in place, which needs a `Mutex`.
        let share = match capture_ty(cx, c, &name).map(|t| cx.ty.kind(t).clone()) {
            Some(TyKind::Int(_) | TyKind::Float(_)) => format!(
                "share it with `shared`: `const {name} = shared(...)` and `{name}.add(1)` / `{name}.set(v)`"
            ),
            Some(TyKind::Str | TyKind::Bool | TyKind::Literal(_)) => format!(
                "share it with `shared`: `const {name} = shared(...)` and `{name}.get()` / `{name}.set(v)`"
            ),
            _ => format!(
                "share it in a `Mutex`: `const {name} = shared(new Mutex(...))`, changed with `{name}.with(...)`"
            ),
        };
        cx.error(d.with_note(format!(
            "{runs}, with its own copy of what it captured: pass `{name}` as a parameter instead, or {share}"
        )));
    }
}

/// The type closure `c` captured the variable `name` with.
fn capture_ty(cx: &Ctx, c: DefId, name: &str) -> Option<TyId> {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return None;
    };
    f.captures
        .iter()
        .map(|cap| &f.body.locals[cap.inner.0 as usize])
        .find(|l| l.name == name)
        .map(|l| l.ty)
}
