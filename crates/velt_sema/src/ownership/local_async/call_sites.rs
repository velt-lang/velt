//! Per call site (#2756): a function called where the callee is unknown (a closure literal or a
//! named function used as a value, through a function value; a method, through a virtual,
//! interface or bounded call) whose parameter crosses makes the argument at that position cross at
//! each call that may reach it, instead of making the parameter's type crossing for the whole
//! program. A closure held only in locals is the callee only of the calls its value reaches
//! ([`held_in_locals`]).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId};
use crate::ownership::local_closures::walk::{self, Visit};

use super::graph::{Graph, Node, CROSSES};
use super::{instances, outermost, types, Propagation};

/// A call through a function value.
pub(super) struct Indirect {
    /// The callee's type.
    pub ty: TyId,
    /// The node of the callee's value (`Node::Callee`).
    pub callee: usize,
    pub args: Vec<usize>,
    pub span: Span,
    /// The call is in a library (std or a package the program depends on).
    pub library: bool,
    /// The function whose closures alone the callee may be (a variable of
    /// `crate::defined_accessors`), if known.
    pub only: Option<DefId>,
}

/// Where values go that the graph's edges don't show, for [`held_in_locals`].
#[derive(Default)]
pub(super) struct Held {
    /// The functions used as values (`E::FnRef`), which may be called with any type arguments,
    /// and those named by code outside every function body (module-level and field initializers,
    /// which the graph does not walk): their results go where the graph does not follow.
    pub referenced: FxHashSet<DefId>,
    /// Nodes whose values are used where the graph does not follow them on: a call's or
    /// comparison's operand, but also a `match` scrutinee its arms bind anew (`f?.(x)`,
    /// `a ?? b`), a destructured value or a started promise `spawn` hands back through its
    /// handle (`await spawn(p)`). No flag: only [`held_in_locals`] stops there.
    pub escapes: Vec<usize>,
    /// Per function called by name: whether every such call reads its result through
    /// `Node::Ret` (none passes more arguments than it has parameters).
    pub direct: FxHashMap<DefId, bool>,
}

/// A closure whose declared parameter crosses (the prelude's `resolve`, a callback that spawns
/// its argument) is called through function values: the argument at that position of every
/// such call whose callee may be the closure crosses too. Returns whether a flag changed.
///
/// A closure whose value reaches neither the heap, nor an unknown callee, nor a boundary is
/// held only by the locals, parameters and results the graph follows ([`held_in_locals`]): it
/// is the callee only of the calls its value flows to, not of every call of a function type its
/// own matches (`Object.defineProperty`'s setter `(v: T) => void`, a `resolve`, is not a
/// stream's `start(controller)` in std).
pub(super) fn crossing_params(cx: &mut Ctx, g: &Graph, p: &mut Propagation) -> bool {
    let mut changed = crossing_method_args(cx, g, p);
    let mut found = vec![];
    let mut fwd: Option<Forward> = None;
    // A named function used as a value, called through function values, with its type where it is
    // used (#2756).
    for &(d, ty) in &g.fn_values {
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            continue;
        };
        // The arguments of a call through the value start at its declared parameters: after any
        // captures and a method's `this`, which the call doesn't pass.
        let skip = f.captures.len() + usize::from(f.self_ty.is_some());
        let declared: Vec<_> = f.params.iter().skip(skip).map(|p| p.local).collect();
        for (k, local) in declared.into_iter().enumerate() {
            if let Some(&i) = g.ids.get(&Node::Local(d, local)) {
                if p.flags[i] & CROSSES != 0 {
                    found.push((ty, k, p.why[i], d, None));
                }
            }
        }
    }
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
                let reached = held_in_locals(g, fwd.get_or_insert_with(|| forward(cx, g)), n);
                for ty in instances(cx, g, c, g.tys[n]) {
                    found.push((ty, k, p.why[i], c, reached.clone()));
                }
            }
        }
    }
    // A closure made where no body the graph walks makes it (a static field's or an imported
    // module's initializer) has no literal node, and its calls aren't known: a parameter of it
    // that crosses makes the matching argument of every call of its type cross.
    for (i, node) in g.nodes.iter().enumerate() {
        let Node::Local(c, local) = *node else {
            continue;
        };
        if p.flags[i] & CROSSES == 0 || g.ids.contains_key(&Node::Lit(c)) {
            continue;
        }
        if cx
            .try_fn(c)
            .is_none_or(|fi| fi.kind != crate::defs::FnKind::Closure)
        {
            continue;
        }
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        let declared = &f.params[f.captures.len().min(f.params.len())..];
        let Some(k) = declared.iter().position(|p| p.local == local) else {
            continue;
        };
        let params: Vec<_> = declared.iter().map(|p| p.ty).collect();
        let (ret, never) = (f.ret, cx.ty.never);
        let ty = cx.ty.intern(crate::hir::TyKind::FnPtr {
            params,
            ret,
            throws: never,
            rest: false,
        });
        found.push((ty, k, p.why[i], c, None));
    }
    for (ty, k, why, c, reached) in found {
        for call in &g.indirect {
            // A call through a variable holding only closures of one function
            // (`crate::defined_accessors`) reaches no closure of another.
            if (call.only).is_some_and(|d| outermost(g, d) != outermost(g, c)) {
                continue;
            }
            if reached.as_ref().is_some_and(|r| !r.contains(&call.callee)) {
                continue;
            }
            if let Some(&a) = call.args.get(k) {
                if p.flags[a] & CROSSES == 0 && types::may_be(cx, ty, call.ty) {
                    let arg_why = match why {
                        Some(w) => match w.step(false, Some((call.span, call.library))) {
                            Some(w) => Some(w),
                            None => continue,
                        },
                        None => None,
                    };
                    p.set(a, CROSSES, arg_why);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// A virtual, interface or bounded call whose argument a method it may reach lets cross (its
/// parameter at that position crosses): the argument crosses too (#2756). A call whose methods
/// aren't known (`Graph::dispatched_calls`, empty) takes every dispatched method. Returns whether a
/// flag changed.
fn crossing_method_args(cx: &Ctx, g: &Graph, p: &mut Propagation) -> bool {
    let mut changed = false;
    for (targets, args, span, library) in &g.dispatched_calls {
        let all: &[DefId] = match targets.is_empty() {
            true => &g.dispatched,
            false => targets,
        };
        for (pos, a) in args.iter().enumerate() {
            let Some(a) = *a else { continue };
            if p.flags[a] & CROSSES != 0 {
                continue;
            }
            // The method's parameters start with `this`, the receiver at position 0.
            let why = all.iter().find_map(|m| {
                let Some(Def::Fn(f)) = &cx.defs[m.0 as usize] else {
                    return None;
                };
                let local = f.params.get(pos)?.local;
                let &i = g.ids.get(&Node::Local(*m, local))?;
                (p.flags[i] & CROSSES != 0).then_some(p.why[i])
            });
            let Some(why) = why else { continue };
            let arg_why = match why {
                Some(w) => match w.step(false, Some((*span, *library))) {
                    Some(w) => Some(w),
                    None => continue,
                },
                None => None,
            };
            p.set(a, CROSSES, arg_why);
            changed = true;
        }
    }
    changed
}

/// Per node, the nodes its values flow into: the graph's edges forward, and a closure's
/// capture locals back to the variables they capture, since a closure that assigns a captured
/// variable changes the enclosing function's (a cell, `crate::moves`). A function's result is a
/// sink unless the graph follows it to every caller ([`results_followed`]).
fn forward(cx: &Ctx, g: &Graph) -> Forward {
    let mut edges = vec![vec![]; g.nodes.len()];
    for (m, srcs) in g.srcs.iter().enumerate() {
        for &(s, _) in srcs {
            edges[s].push(m);
        }
    }
    for caps in g.captures.values() {
        for &i in caps {
            for &(o, _) in &g.srcs[i] {
                edges[i].push(o);
            }
        }
    }
    let mut sink = vec![false; g.nodes.len()];
    for &(n, _, _) in &g.seeds {
        sink[n] = true;
    }
    for &n in &g.held.escapes {
        sink[n] = true;
    }
    for (n, node) in g.nodes.iter().enumerate() {
        if let Node::Ret(d) = *node {
            sink[n] |= !results_followed(cx, g, d);
        }
    }
    Forward { edges, sink }
}

/// Does every caller of `d` read its result through its `Node::Ret`? Only for a function
/// called by name, with no more arguments than parameters, and never otherwise: not a method
/// (dispatch, an interface), a closure or a function used as a value (calls through function
/// values), nor a function nothing calls (`main`, what the host calls).
fn results_followed(cx: &Ctx, g: &Graph, d: DefId) -> bool {
    let free = matches!(&cx.defs[d.0 as usize], Some(Def::Fn(f)) if f.self_ty.is_none());
    free && !g.parent.contains_key(&d)
        && !g.held.referenced.contains(&d)
        && g.held.direct.get(&d).copied().unwrap_or(false)
}

/// The graph's edges forward ([`forward`]), and the nodes whose values reach a sink directly.
struct Forward {
    edges: Vec<Vec<usize>>,
    sink: Vec<bool>,
}

/// The nodes the value of closure literal node `n` may flow into (`n` included), if they are
/// all locals, parameters, results and callees the graph follows: `None` when the value reaches
/// a sink (the heap, a boundary, an unknown callee), is used anywhere but as a callee or in
/// what the graph follows on (`Held::escapes`), or is an argument of a call through a function
/// value or a dispatched call, whose parameter the graph does not follow. A closure that
/// captures it holds it in a capture local, which only that closure's body reads: followed like
/// any local, so the flags the capturing closure takes (`Propagation::run`) don't count here.
fn held_in_locals(g: &Graph, fwd: &Forward, n: usize) -> Option<FxHashSet<usize>> {
    let mut seen = FxHashSet::default();
    let mut work = vec![n];
    while let Some(m) = work.pop() {
        if !seen.insert(m) {
            continue;
        }
        if fwd.sink[m] || matches!(g.nodes[m], Node::Arg(..) | Node::DArg(..)) {
            return None;
        }
        work.extend(fwd.edges[m].iter().copied().filter(|x| !seen.contains(x)));
    }
    Some(seen)
}

/// Collects into `out` the functions that code outside every function body (module-level and
/// field initializers) calls by name or uses as a value: called from where the graph does not
/// follow, with no type arguments recorded.
pub(super) fn named_outside_bodies(cx: &Ctx, out: &mut FxHashSet<DefId>) {
    let mut named = Named(out);
    for info in cx.info.iter() {
        match info {
            DefInfo::Global(gi) => {
                if let Some(init) = &gi.init {
                    walk::expr(init, &mut named);
                }
            }
            DefInfo::Adt(a) => {
                let defaults = a.fields.iter().filter_map(|f| f.default.as_ref());
                for v in defaults.chain(a.redefined.iter().filter_map(|r| r.init.as_ref())) {
                    walk::expr(v, &mut named);
                }
            }
            _ => {}
        }
    }
    for def in cx.defs.iter().flatten() {
        match def {
            Def::Global(gd) => walk::expr(&gd.init, &mut named),
            Def::Adt(a) => {
                for v in a.fields.iter().filter_map(|f| f.default.as_ref()) {
                    walk::expr(v, &mut named);
                }
                for init in &a.inherited_inits {
                    walk::expr(&init.init, &mut named);
                }
            }
            _ => {}
        }
    }
}

/// Collects the functions an expression calls by name or uses as a value.
struct Named<'a>(&'a mut FxHashSet<DefId>);

impl Visit for Named<'_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::FnRef(d, _)
            | E::Call {
                callee: Callee::Def(d, _),
                ..
            } => _ = self.0.insert(*d),
            _ => {}
        }
    }
}
