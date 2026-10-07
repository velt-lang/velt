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
//!   (`spawn(f())`) or an argument of one (`spawn(run(f))`);
//! - the argument of `shared(...)` or `new Mutex(...)`, a channel send, a value settling a
//!   promise (`Intrinsic::Transfer`) or an HTTP handler (`Intrinsic::HttpHandler`);
//! - passed directly to a function value, an interface method or an overridden method, whose
//!   callee is unknown and may keep it;
//! - through locals, parameters (from every direct caller), return values, and the captures of
//!   a closure that is itself non-local (or passed to an unknown callee);
//! - through the heap, by type ([`types`]): a closure stored in a field, element or map value
//!   is non-local when a value crossing a boundary has a type reaching a function type of the
//!   same shape.
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
use crate::hir::{Def, DefId, TyId};

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
            let why = fns
                .iter()
                .find(|(t, _)| types::may_be(cx, g.tys[n], **t))
                .map(|(_, w)| *w);
            if let Some(w) = why {
                p.set(n, CROSSES, Some(w));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (n, node) in g.nodes.iter().enumerate() {
        let Node::Lit(c) = *node else { continue };
        let why = p.why[n][0].or(p.why[n][1]);
        let local = p.flags[n] & (CROSSES | INDIRECT) == 0;
        finish(cx, c, local, why);
    }
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
    let mutated = std::mem::take(&mut cx.fn_info_mut(c).mutated_captures);
    for (name, at) in mutated {
        let mut d = Diagnostic::error(
            format!("this async closure modifies captured `{name}`, so it must stay on the task that created it"),
            at,
        );
        if let Some(w) = why {
            let label = reaches(cx, &w);
            d = d.with_label(w.span, label);
        }
        cx.error(d.with_note(format!(
            "a spawned task, an HTTP handler or a `shared` value may run the closure on another thread: pass `{name}` as a parameter instead, or share it with `shared`: `const {name} = shared(...)` and `{name}.add(n)` / `{name}.set(v)`, or `shared(new Mutex(...))` with `.with(...)`"
        )));
    }
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
        Some(t) => format!("but it is stored in a `{}`, which {place}", cx.display(t)),
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
    }
}
