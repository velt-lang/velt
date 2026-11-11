//! Per call site (#2756): a function called where the callee is unknown (a closure literal or a
//! named function used as a value, through a function value; a method, through a virtual,
//! interface or bounded call) whose parameter crosses makes the argument at that position cross at
//! each call that may reach it, instead of making the parameter's type crossing for the whole
//! program.

use crate::ctx::Ctx;
use crate::hir::{Def, DefId};

use super::graph::{Graph, Node, CROSSES};
use super::{instances, outermost, types, Propagation};

/// A closure whose declared parameter crosses (the prelude's `resolve`, a callback that spawns
/// its argument) is called through function values: the argument at that position of every
/// such call whose callee may be the closure crosses too. Returns whether a flag changed.
pub(super) fn crossing_params(cx: &mut Ctx, g: &Graph, p: &mut Propagation) -> bool {
    let mut changed = crossing_method_args(cx, g, p);
    let mut found = vec![];
    // A named function used as a value, called through function values, with its type where it is
    // used (#2756).
    for &(d, ty) in &g.fn_values {
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            continue;
        };
        let declared: Vec<_> = f.params.iter().map(|p| p.local).collect();
        for (k, local) in declared.into_iter().enumerate() {
            if let Some(&i) = g.ids.get(&Node::Local(d, local)) {
                if p.flags[i] & CROSSES != 0 {
                    found.push((ty, k, p.why[i], d));
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
                for ty in instances(cx, g, c, g.tys[n]) {
                    found.push((ty, k, p.why[i], c));
                }
            }
        }
    }
    for (ty, k, why, c) in found {
        for (callee, args, span, library, only) in &g.indirect {
            // A call through a variable holding only closures of one function
            // (`crate::defined_accessors`) reaches no closure of another.
            if only.is_some_and(|d| outermost(g, d) != outermost(g, c)) {
                continue;
            }
            if let Some(&a) = args.get(k) {
                if p.flags[a] & CROSSES == 0 && types::may_be(cx, ty, *callee) {
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
