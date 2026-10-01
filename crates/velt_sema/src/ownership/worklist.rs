//! Scheduling of the ownership fixpoint (`super::infer`): which bodies and dispatch groups to
//! visit, and in what order.
//!
//! A body's call-site patching and inference read only the body itself, the modes of the
//! functions it calls (vtable entries and interface slots included), and the closures and
//! named functions it references. So when a function's state changes, only the bodies that
//! read it (and the dispatch groups it belongs to) are visited again — never the whole
//! program. Bodies are visited callees (and closures) first, so a mode change travels up a
//! call chain in one pass instead of one whole-program round per call level; only recursion
//! and dynamic dispatch send work back to bodies already visited.

use std::collections::{BTreeSet, HashMap};

use crate::ctx::Ctx;
use crate::hir::{DefId, Expr, ExprKind as E};
use crate::visit;

use super::infer::with_body;
use super::mutation::Dispatch;
use super::patch::call_reads;

/// Pending work of the ownership fixpoint.
pub(super) struct Worklist {
    /// Bodies in visiting order (callees before the bodies that read them).
    order: Vec<DefId>,
    /// Positions (into `order`) of the bodies that read a def, the def's own body included.
    readers: HashMap<DefId, Vec<usize>>,
    /// Dispatch groups (`Dispatch`) that contain a def.
    groups: HashMap<DefId, Vec<usize>>,
    bodies: BTreeSet<usize>,
    joins: BTreeSet<usize>,
}

impl Worklist {
    /// Everything pending, with the read dependencies of every body in `fns`.
    pub(super) fn new(cx: &mut Ctx, fns: &[DefId], dispatch: &Dispatch) -> Worklist {
        let reads: Vec<Vec<DefId>> = fns.iter().map(|&d| reads_of(cx, d)).collect();
        let order = callees_first(fns, &reads, dispatch);
        let at: HashMap<DefId, usize> = fns.iter().copied().zip(0..).collect();
        let mut readers: HashMap<DefId, Vec<usize>> = HashMap::new();
        for (p, d) in order.iter().enumerate() {
            readers.entry(*d).or_default().push(p);
            for &r in reads[at[d]].iter().filter(|r| *r != d) {
                readers.entry(r).or_default().push(p);
            }
        }
        Worklist {
            bodies: (0..order.len()).collect(),
            joins: (0..dispatch.len()).collect(),
            order,
            readers,
            groups: dispatch.membership(),
        }
    }

    /// `d`'s modes, body or captures changed: revisit its readers and dispatch groups.
    pub(super) fn changed(&mut self, d: DefId) {
        self.bodies
            .extend(self.readers.get(&d).into_iter().flatten().copied());
        self.joins
            .extend(self.groups.get(&d).into_iter().flatten().copied());
    }

    /// The next body to visit (earliest in visiting order), taken off the list.
    pub(super) fn next_body(&mut self) -> Option<DefId> {
        self.bodies.pop_first().map(|p| self.order[p])
    }

    /// The next dispatch group to join, taken off the list.
    pub(super) fn next_join(&mut self) -> Option<usize> {
        self.joins.pop_first()
    }
}

/// The defs whose state patching and inferring `d`'s body reads, sorted and deduplicated.
fn reads_of(cx: &mut Ctx, d: DefId) -> Vec<DefId> {
    let mut reads = vec![];
    with_body(cx, d, |cx, f| {
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            E::Closure(r) | E::FnRef(r, _) => reads.push(*r),
            _ => call_reads(cx, e, &mut reads),
        });
        false
    });
    reads.sort_unstable();
    reads.dedup();
    reads
}

/// `fns` in depth-first post-order over the read edges, roots in definition order: callees
/// before their callers. A call through a dispatch slot reads every method of the slot: the
/// graph has a node per dispatch group (after the `fns` nodes) with an edge to each method,
/// which keeps it linear in size however many bodies call through a slot.
fn callees_first(fns: &[DefId], reads: &[Vec<DefId>], dispatch: &Dispatch) -> Vec<DefId> {
    let n = fns.len();
    let at: HashMap<DefId, usize> = fns.iter().copied().zip(0..).collect();
    let slots = dispatch.slots();
    let successors = |node: usize| -> Vec<usize> {
        let Some(reads) = reads.get(node) else {
            let methods = dispatch.members(node - n);
            return methods.iter().filter_map(|m| at.get(m).copied()).collect();
        };
        let direct = reads.iter().filter_map(|r| at.get(r).copied());
        let through = reads
            .iter()
            .flat_map(|r| slots.get(r).into_iter().flatten().map(|g| n + g));
        direct.chain(through).collect()
    };
    let mut seen = vec![false; n + dispatch.len()];
    let mut order = Vec::with_capacity(n);
    for root in 0..n {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        // Iterative: call chains can be as deep as the program is long.
        let mut stack = vec![(root, successors(root), 0)];
        while let Some(top) = stack.last_mut() {
            match top.1.get(top.2).copied() {
                Some(s) => {
                    top.2 += 1;
                    if !seen[s] {
                        seen[s] = true;
                        stack.push((s, successors(s), 0));
                    }
                }
                None => {
                    if let Some(&d) = fns.get(top.0) {
                        order.push(d);
                    }
                    stack.pop();
                }
            }
        }
    }
    order
}
