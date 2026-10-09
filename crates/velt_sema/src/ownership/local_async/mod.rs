//! Local async closures (#208, docs/reference/functions.md "Captures"): an async closure that
//! never reaches a thread boundary captures like a sync closure, so it may modify what it
//! captured and the enclosing code may assign the variables it captured, as in JavaScript.
//!
//! Every call of an async closure is a started promise on the caller's task, which never runs
//! at the same time as the task's other code. What makes sharing unsafe is another thread: a
//! spawned task, an HTTP handler running on every core, a `shared(...)` value, a channel, or a
//! promise settled for another task. An async closure is **non-local** when its value may
//! reach one of them ([`graph`]):
//! - spawned: a spawned closure literal (`spawn(async () => …)`), the callee of a spawned call
//!   (`spawn(f())`), an argument of one (`spawn(run(f))`) or what the spawned function returns;
//! - the argument of `shared(...)` or `new Mutex(...)`, a channel send, a value settling a
//!   promise (`Intrinsic::Transfer`) or an HTTP handler (`Intrinsic::HttpHandler`);
//! - passed directly to a function value, an interface method or an overridden method, whose
//!   callee is unknown and may keep it;
//! - through locals, parameters (from every direct caller), return values, and the captures of
//!   a closure that is itself non-local (or passed to an unknown callee);
//! - an argument of a call through a function value that may be a closure whose parameter
//!   crosses (`resolve(x)`), matched by type in each instantiation of the closure's function;
//! - through the heap, by type ([`types`]): a closure stored in a field, element or map value
//!   is non-local when a value crossing a boundary has a type reaching a function type of the
//!   same shape. A value of unknown origin keeps the type it had where it flowed in, so a
//!   generic parameter that crosses brings its callers' concrete types.
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

mod graph;
mod types;

use std::collections::{HashMap, HashSet};

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, TyId, TyKind};

use graph::{Boundary, Graph, Node, Why, CROSSES, INDIRECT, STORED};

/// Decide which async closures are local, mark them (`FnDef::shares_captures`), and report the
/// non-local ones that modify a captured variable.
pub(crate) fn infer_local_async(cx: &mut Ctx) {
    let g = graph::build(cx);
    let mut p = Propagation::new(&g);
    for &(n, f, why) in &g.seeds {
        p.set(n, f, why);
    }
    let mut seen = HashSet::new();
    let mut fns: HashMap<TyId, Why> = HashMap::new();
    let mut rooted = 0;
    loop {
        p.run(cx, &g);
        types::crossing_fns(cx, &p.roots[rooted..], &mut seen, &mut fns);
        rooted = p.roots.len();
        let mut changed = false;
        for (n, node) in g.nodes.iter().enumerate() {
            if !matches!(node, Node::Lit(_))
                || p.flags[n] & CROSSES != 0
                || p.flags[n] & STORED == 0
            {
                continue;
            }
            // A callback wrapper of an async closure (`body/expr/callback.rs`) is matched by the
            // async closure's type, or by its own type only where nothing generic stands for it:
            // `input.onChange = async (v) => …` in a `(v: string) => void` field is not the
            // prelude's `resolve`, a `(T) => void`, but is any `(v: string) => void` that crosses.
            let wrapped = match node {
                Node::Lit(c) => cx.callback_wrappers.get(c).copied(),
                _ => None,
            };
            // A sync closure is matched the same strict way: `resolve`'s `(T) => void` stands for
            // no sync callback the user stores.
            let sync = match node {
                Node::Lit(c) => matches!(&cx.defs[c.0 as usize], Some(Def::Fn(f)) if !f.is_async),
                _ => false,
            };
            let matches = |t: TyId| match wrapped {
                Some(inner) => {
                    types::may_be(cx, inner, t)
                        || (!types::mentions_param(cx, t) && types::may_be(cx, g.tys[n], t))
                }
                None if sync => !types::mentions_param(cx, t) && types::may_be(cx, g.tys[n], t),
                None => types::may_be(cx, g.tys[n], t),
            };
            // Several crossing types may match: report one in user code, the earliest.
            let why = fns
                .iter()
                .filter(|(t, _)| matches(**t))
                .map(|(_, w)| *w)
                .min_by_key(|w| (w.std, w.span.file, w.span.lo));
            if let Some(w) = why {
                p.set(n, CROSSES, Some(w));
                changed = true;
            }
        }
        changed |= crossing_params(cx, &g, &mut p);
        if !changed {
            break;
        }
    }
    let handler_reads = handler_fn_reads(cx, &g, &p);
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Lit(c) = *node else { continue };
        let why = p.why[n][0].or(p.why[n][1]);
        let local = p.flags[n] & (CROSSES | INDIRECT) == 0;
        finish(cx, c, local, why);
        let crosses = p.flags[n] & CROSSES != 0;
        sync_crossing(cx, c, crosses, p.why[n][0], g.tys[n], &handler_reads);
    }
}

/// A closure whose declared parameter crosses (the prelude's `resolve`, a callback that spawns
/// its argument) is called through function values: the argument at that position of every
/// such call whose callee may be the closure crosses too. Returns whether a flag changed.
fn crossing_params(cx: &mut Ctx, g: &Graph, p: &mut Propagation) -> bool {
    let mut found = vec![];
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Lit(c) = *node else { continue };
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        let declared: Vec<_> = f
            .params
            .iter()
            .skip(f.captures.len())
            .map(|p| p.local)
            .collect();
        for (k, local) in declared.into_iter().enumerate() {
            let Some(&i) = g.ids.get(&Node::Local(c, local)) else {
                continue;
            };
            if p.flags[i] & CROSSES != 0 {
                for ty in instances(cx, g, c, g.tys[n]) {
                    found.push((ty, k, p.why[i][0]));
                }
            }
        }
    }
    let mut changed = false;
    for (ty, k, why) in found {
        for (callee, args, span, std) in &g.indirect {
            if let Some(&a) = args.get(k) {
                if p.flags[a] & CROSSES == 0 && types::may_be(cx, ty, *callee) {
                    p.set(a, CROSSES, why.map(|w| w.through(Some((*span, *std)))));
                    changed = true;
                }
            }
        }
    }
    changed
}

/// The types closure literal `c` (of type `ty`) has in the instantiations of the generic
/// function that creates it: `(value: T) => void` in `withResolvers<T>` is `(value: Job) =>
/// void` where the program calls `withResolvers<Job>()`. Generic parameters of callers that
/// are generic themselves are followed to their own callers; where that gives no answer (or too
/// many), the generic type itself, whose parameters match anything; a generic standard library
/// function the program never calls gives none.
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
        // A generic function of the standard library that the program never calls directly
        // makes no such closure (its methods are called with their type arguments too).
        let std = cx.try_fn(f).is_some_and(|i| cx.scopes[i.module].is_std);
        if sites.is_empty() && std {
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

/// The types of the function values that code an HTTP handler runs reads from a field or an
/// element: the handler closures (and the function values flowing to them as values), the
/// functions they call directly, and the closures created in those, transitively.
fn handler_fn_reads(cx: &Ctx, g: &Graph, p: &Propagation) -> Vec<TyId> {
    let mut work: Vec<DefId> = vec![];
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Lit(c) = *node else { continue };
        let runs = p.flags[n] & CROSSES != 0
            && p.why[n][0].is_some_and(|w| {
                w.boundary == Boundary::Handler
                    && w.via
                        .is_none_or(|t| matches!(cx.ty.kind(t), TyKind::FnPtr { .. }))
            });
        if runs {
            work.push(c);
        }
    }
    let mut children: HashMap<DefId, Vec<DefId>> = HashMap::new();
    for (&c, &parent) in &g.parent {
        children.entry(parent).or_default().push(c);
    }
    let mut seen: HashSet<DefId> = HashSet::new();
    let mut out = vec![];
    while let Some(d) = work.pop() {
        if !seen.insert(d) {
            continue;
        }
        out.extend(g.fn_reads.get(&d).into_iter().flatten().copied());
        work.extend(g.callees.get(&d).into_iter().flatten().copied());
        work.extend(children.get(&d).into_iter().flatten().copied());
    }
    out.sort();
    out.dedup();
    out
}

/// A sync closure `c` that an HTTP handler runs (`serve`'s handler, or a function value the
/// handler calls) runs on several threads at once with a copy of what it captured, like an
/// async handler:
/// modifying a capture is the same error, wherever the closure was written (a variable, a
/// field, a function's result). Other crossings keep their copy semantics (a sync closure
/// handed to a spawned task counts on its own copy, `spawn_fn_value_copied_per_task`), and
/// only a definite crossing counts (a closure passed to an unknown callee is called in place).
fn sync_crossing(
    cx: &mut Ctx,
    c: DefId,
    crosses: bool,
    why: Option<Why>,
    ty: TyId,
    handler_reads: &[TyId],
) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    // The handler runs it: the handler itself, a function value it gets as a value (a
    // variable, a function's result), or one of the type of a function value that handler code
    // reads from a field or an element (to call it or pass it on: `i.onChange("x")`). A
    // closure only stored in an object the handler captured, of a type handler code never
    // reads (`w.onClick` of a captured `w`), keeps the copy semantics it has on main.
    let runs = why.is_some_and(|w| {
        w.boundary == Boundary::Handler
            && (w
                .via
                .is_none_or(|t| matches!(cx.ty.kind(t), TyKind::FnPtr { .. }))
                || handler_reads
                    .iter()
                    .any(|r| !types::mentions_param(cx, *r) && types::may_be(cx, ty, *r)))
    });
    if f.is_async || f.is_generator || !crosses || !runs {
        return;
    }
    report_mutated(cx, c, why, "closure");
}

/// Record what was decided for async closure `c`.
fn finish(cx: &mut Ctx, c: DefId, local: bool, why: Option<Why>) {
    let Some(Def::Fn(f)) = &mut cx.defs[c.0 as usize] else {
        return;
    };
    if !f.is_async || f.is_generator {
        return;
    }
    if local {
        f.shares_captures = true;
        return;
    }
    report_mutated(cx, c, why, "async closure");
}

/// The label naming where a non-local closure leaves its task.
fn reaches(cx: &Ctx, w: &Why) -> String {
    let place = match w.boundary {
        Boundary::Spawn => "reaches `spawn` here",
        Boundary::Handler => "handles HTTP requests here, which run on several threads",
        Boundary::Shared => "reaches `shared(...)` here",
        Boundary::Mutex => "is put in a `Mutex` here, which other threads may lock",
        Boundary::Channel => "is sent on a channel here",
        Boundary::Settle => "settles a promise here, which another task may await",
        Boundary::FnValue => {
            "is passed to a function value or an interface method here, which may keep it"
        }
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

/// Flags of every node, with why each was set (`CROSSES`, `INDIRECT`), propagated from a
/// node to the nodes its values come from.
struct Propagation {
    flags: Vec<u8>,
    why: Vec<[Option<Why>; 2]>,
    work: Vec<(usize, u8)>,
    roots: Vec<(TyId, Why)>,
}

impl Propagation {
    fn new(g: &Graph) -> Self {
        Propagation {
            flags: vec![0; g.nodes.len()],
            why: vec![[None, None]; g.nodes.len()],
            work: vec![],
            roots: g.roots.clone(),
        }
    }

    fn set(&mut self, n: usize, f: u8, why: Option<Why>) {
        if self.flags[n] & f != 0 {
            return;
        }
        self.flags[n] |= f;
        match f {
            CROSSES => self.why[n][0] = why,
            INDIRECT => self.why[n][1] = why,
            _ => {}
        }
        self.work.push((n, f));
    }

    fn run(&mut self, cx: &Ctx, g: &Graph) {
        while let Some((n, f)) = self.work.pop() {
            let why = match f {
                CROSSES => self.why[n][0],
                INDIRECT => self.why[n][1],
                _ => None,
            };
            if let Some(w) = why {
                self.root(cx, g, n, f, w);
            }
            for &(m, site) in &g.srcs[n] {
                self.set(m, f, why.map(|w| w.through(site)));
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
        let fn_like = types::fn_like(cx, ty);
        let rooted = match f {
            CROSSES => g.unknown[n] || !fn_like,
            INDIRECT => g.unknown[n] && fn_like,
            _ => false,
        };
        if rooted && !matches!(g.nodes[n], Node::Lit(_)) {
            self.roots.push((ty, w));
        }
        // What flowed in from places the graph does not follow, with the types it had there:
        // a generic parameter's node gets its callers' concrete types.
        for &(t, site) in &g.inflows[n] {
            let take = match f {
                CROSSES => true,
                INDIRECT => types::fn_like(cx, t),
                _ => false,
            };
            if take && t != ty {
                self.roots.push((t, w.through(site)));
            }
        }
    }
}

/// "this `what` modifies captured `x`" for each capture closure `c` modifies, naming the
/// boundary `why` it reaches.
fn report_mutated(cx: &mut Ctx, c: DefId, why: Option<Why>, what: &str) {
    let mutated = std::mem::take(&mut cx.fn_info_mut(c).mutated_captures);
    let closure = cx.def_spans.get(c.0 as usize).copied();
    // A sync arrow passed to `serve` is checked as an async one: call it what it is.
    let what = match closure {
        Some(span) if cx.sync_handlers.contains(&span) => "handler",
        _ => what,
    };
    for (name, at) in mutated {
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
            Some(Boundary::Handler) => "requests run it on several threads at once",
            Some(Boundary::Shared | Boundary::Mutex) => {
                "every thread holding the `shared` value may call it"
            }
            Some(Boundary::Channel) => "the task receiving it runs it on another thread",
            Some(Boundary::Settle) => "the task awaiting the promise may run it on another thread",
            Some(Boundary::FnValue) | None => {
                "the function it is passed to may keep it and run it on another thread"
            }
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
