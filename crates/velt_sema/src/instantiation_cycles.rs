//! Generic recursion whose type arguments grow (`f<T>` calling `f<T[]>`), directly or through a
//! cycle of generic functions. Velt compiles each instantiation separately, so such a program
//! has infinitely many instantiations: lowering would never finish, and neither would the
//! passes that propagate requirements through generic calls (`throws`, `record_keys`, `json`).
//! TypeScript reports the same programs as "excessively deep and possibly infinite".
//!
//! The check is the expansion test of generic graphs (ECMA-335 II.9.2): a node per type
//! parameter of a function, and for each place a body instantiates a generic function, an edge
//! from every parameter of the caller to the callee's parameter whose argument mentions it. The
//! edge *grows* when that argument is more than the bare parameter (`T[]`, `Box<T>`). A program
//! has finitely many instantiations iff no growing edge lies on a cycle, i.e. inside one
//! strongly connected component. Instantiations come from direct calls (`Callee::Def`), closures
//! (which share their enclosing function's parameters) and the methods a constructed or
//! dynamically converted class runs ([`crate::dispatch`]).

use std::collections::{HashMap, HashSet, VecDeque};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::Dispatch;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::types::collect_params;
use crate::visit;

/// Type parameter `.1` of function `.0`.
type Node = (DefId, u32);

/// One place a body instantiates another definition.
enum Site {
    /// A generic function or method, with its type arguments.
    Call(DefId, Vec<TyId>),
    /// A closure: it runs with its enclosing function's type arguments.
    Closure(DefId),
    /// A class value built or converted to an interface: its methods are instantiated with the
    /// type's arguments.
    Object(TyId),
}

/// An edge of the expansion graph and the call that makes it.
struct Edge {
    from: Node,
    to: Node,
    grows: bool,
    callee: DefId,
    targs: Vec<TyId>,
    span: Span,
}

/// Reports every call that instantiates a generic function with ever-growing type arguments
/// (`true`: something was reported; the requirement passes must not run then).
pub(crate) fn check(cx: &mut Ctx) -> bool {
    let sites = collect(cx);
    let edges = edges(cx, sites);
    if !edges.iter().any(|e| e.grows) {
        return false;
    }
    let graph = Graph::new(&edges);
    let comp = graph.components();
    let mut reported: HashSet<Span> = HashSet::new();
    for (i, e) in edges.iter().enumerate() {
        let (a, b) = (graph.index[&e.from], graph.index[&e.to]);
        if e.grows && comp[a] == comp[b] && reported.insert(e.span) {
            let back = graph.path(&edges, b, a, &comp);
            report(cx, &edges, i, &back);
        }
    }
    !reported.is_empty()
}

/// The instantiation sites of every generic function body (a function without type parameters
/// cannot be part of a growing cycle).
fn collect(cx: &mut Ctx) -> Vec<(DefId, Vec<(Site, Span)>)> {
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        if f.generics == 0 {
            continue;
        }
        let mut sites = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let site = match &e.kind {
                E::Call {
                    callee: Callee::Def(d, targs),
                    ..
                } if !targs.is_empty() => Site::Call(*d, targs.clone()),
                E::Closure(c) => Site::Closure(*c),
                E::New { .. } => Site::Object(e.ty),
                E::ToDyn { expr, .. } => Site::Object(expr.ty),
                _ => return,
            };
            sites.push((site, e.span));
        });
        out.push((DefId(i as u32), sites));
    }
    out
}

fn edges(cx: &mut Ctx, sites: Vec<(DefId, Vec<(Site, Span)>)>) -> Vec<Edge> {
    let mut dispatch = Dispatch::default();
    let mut out = vec![];
    for (f, list) in sites {
        for (site, span) in list {
            let calls = match site {
                Site::Call(g, targs) => vec![(g, targs)],
                Site::Closure(c) => {
                    let n = generics(cx, c);
                    vec![(c, (0..n).map(|p| cx.ty.param(p)).collect())]
                }
                Site::Object(t) => object_calls(cx, &mut dispatch, t),
            };
            for (g, targs) in calls {
                call_edges(cx, f, g, &targs, span, &mut out);
            }
        }
    }
    out
}

/// The constructor and dispatched methods a value of type `t` instantiates.
fn object_calls(cx: &mut Ctx, dispatch: &mut Dispatch, t: TyId) -> Vec<(DefId, Vec<TyId>)> {
    let mut calls = dispatch.targets(cx, t);
    if let TyKind::Adt(d, args) = cx.ty.kind(t).clone() {
        if let Some(ctor) = cx.adt(d).and_then(|a| a.ctor) {
            calls.push((ctor, args));
        }
    }
    calls
}

fn call_edges(cx: &Ctx, f: DefId, g: DefId, targs: &[TyId], span: Span, out: &mut Vec<Edge>) {
    for (j, t) in targs.iter().enumerate() {
        let mut ps = vec![];
        collect_params(&cx.ty, *t, &mut ps);
        for p in ps {
            let bare = matches!(cx.ty.kind(*t), TyKind::Param(q) if *q == p);
            out.push(Edge {
                from: (f, p),
                to: (g, j as u32),
                grows: !bare,
                callee: g,
                targs: targs.to_vec(),
                span,
            });
        }
    }
}

fn generics(cx: &Ctx, d: DefId) -> u32 {
    match &cx.defs[d.0 as usize] {
        Some(Def::Fn(f)) => f.generics,
        _ => 0,
    }
}

/// The expansion graph as adjacency lists over dense node indices.
struct Graph {
    index: HashMap<Node, usize>,
    /// Outgoing edges (indices into the edge list) per node.
    out: Vec<Vec<usize>>,
    /// Target node per edge.
    to: Vec<usize>,
}

impl Graph {
    fn new(edges: &[Edge]) -> Graph {
        let mut g = Graph {
            index: HashMap::new(),
            out: vec![],
            to: vec![],
        };
        for e in edges {
            let a = g.node(e.from);
            let b = g.node(e.to);
            g.out[a].push(g.to.len());
            g.to.push(b);
        }
        g
    }

    fn node(&mut self, n: Node) -> usize {
        let next = self.out.len();
        let i = *self.index.entry(n).or_insert(next);
        if i == next {
            self.out.push(vec![]);
        }
        i
    }

    /// The strongly connected component of each node (Tarjan's algorithm, iterative so a long
    /// call chain cannot overflow the stack).
    fn components(&self) -> Vec<usize> {
        let n = self.out.len();
        let (mut low, mut num) = (vec![0usize; n], vec![usize::MAX; n]);
        let (mut comp, mut on_stack) = (vec![usize::MAX; n], vec![false; n]);
        let (mut stack, mut counter, mut comps) = (vec![], 0, 0);
        for root in 0..n {
            if num[root] != usize::MAX {
                continue;
            }
            // Frames: (node, next outgoing edge to visit).
            let mut frames = vec![(root, 0usize)];
            num[root] = counter;
            low[root] = counter;
            counter += 1;
            stack.push(root);
            on_stack[root] = true;
            while let Some(&mut (v, ref mut next)) = frames.last_mut() {
                if let Some(&e) = self.out[v].get(*next) {
                    *next += 1;
                    let w = self.to[e];
                    if num[w] == usize::MAX {
                        num[w] = counter;
                        low[w] = counter;
                        counter += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        frames.push((w, 0));
                    } else if on_stack[w] {
                        low[v] = low[v].min(num[w]);
                    }
                    continue;
                }
                frames.pop();
                if let Some(&(parent, _)) = frames.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == num[v] {
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp[w] = comps;
                        if w == v {
                            break;
                        }
                    }
                    comps += 1;
                }
            }
        }
        comp
    }

    /// The edges of a shortest path from `from` to `to` inside their component.
    fn path(&self, edges: &[Edge], from: usize, to: usize, comp: &[usize]) -> Vec<usize> {
        let mut via: HashMap<usize, usize> = HashMap::new();
        let mut queue = VecDeque::from([from]);
        while let Some(v) = queue.pop_front() {
            if v == to {
                break;
            }
            for &e in &self.out[v] {
                let w = self.to[e];
                if comp[w] == comp[from] && w != from && !via.contains_key(&w) {
                    via.insert(w, e);
                    queue.push_back(w);
                }
            }
        }
        let mut path = vec![];
        let mut at = to;
        while at != from {
            let Some(&e) = via.get(&at) else { break };
            path.push(e);
            at = self.index[&edges[e].from];
        }
        path.reverse();
        path
    }
}

/// Reports growing edge `i`; `back` leads from its callee back to its caller.
fn report(cx: &mut Ctx, edges: &[Edge], i: usize, back: &[usize]) {
    let e = &edges[i];
    let (caller, callee) = (e.from.0, e.callee);
    let names = generic_names(cx, caller);
    let args: Vec<String> = e.targs.iter().map(|t| cx.display_in(*t, &names)).collect();
    let grown = cx.display_in(e.targs[e.to.1 as usize], &names);
    let (f, g) = (fn_name(cx, caller), fn_name(cx, callee));
    let own = format!("{f}<{}>", names.join(", "));
    // A call from a closure is its enclosing function calling.
    let (fq, gq) = (enclosing_fn(cx, caller), enclosing_fn(cx, callee));
    let cycle = if fq == gq {
        format!("`{f}` calls itself with `{grown}`")
    } else {
        let mut through: Vec<String> = vec![];
        let mut seen = vec![fq, gq];
        for &b in back {
            let d = edges[b].to.0;
            let dq = enclosing_fn(cx, d);
            if !seen.contains(&dq) {
                seen.push(dq);
                through.push(fn_name(cx, d));
            }
        }
        let via = match through.as_slice() {
            [] => String::new(),
            ts => format!(" through `{}`", ts.join("`, `")),
        };
        format!("`{f}` calls `{g}` with `{grown}`, which calls `{f}` again{via}")
    };
    cx.error(
        Diagnostic::error(
            format!(
                "instantiating `{g}<{}>` from `{own}` grows without end",
                args.join(", ")
            ),
            e.span,
        )
        .with_note(format!(
            "{cycle}: Velt compiles each instantiation separately, so this recursion never ends; use a non-generic helper or a `JsonValue`"
        )),
    );
}

/// The names of `d`'s type parameters (a closure's are its enclosing function's).
fn generic_names(cx: &Ctx, d: DefId) -> Vec<String> {
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => f.generics.names.clone(),
        _ => vec![],
    }
}

/// The qualified name of `d`, or of the function enclosing it if it is a closure.
fn enclosing_fn<'a>(cx: &'a Ctx, d: DefId) -> &'a str {
    let name = match &cx.defs[d.0 as usize] {
        Some(Def::Fn(f)) => f.name.as_str(),
        _ => "?",
    };
    name.split("::{closure").next().unwrap_or(name)
}

/// A function's name as users wrote it (a closure is named after its enclosing function).
fn fn_name(cx: &Ctx, d: DefId) -> String {
    let name = enclosing_fn(cx, d);
    name.rsplit("::").next().unwrap_or(name).to_string()
}
