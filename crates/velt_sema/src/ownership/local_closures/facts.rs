//! What the parent module asks of an enclosing function's body for each of its closures, found
//! in one walk of the body: which locals it assigns and how it uses them. Asking by walking the
//! body once per closure (and per capture) cost the square of a function that creates many
//! closures.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{Block, Callee, Def, Expr, ExprKind as E, LocalId};

use super::each_expr;

/// The locals a block assigns: as a whole, or through a projection too (`t[0] = 10`,
/// `p.x += 1`), directly or by a closure created in it that captures them.
#[derive(Default)]
pub(super) struct Assigned {
    whole: FxHashSet<LocalId>,
    rooted: FxHashSet<LocalId>,
}

impl Assigned {
    pub(super) fn of(cx: &Ctx, b: &Block) -> Assigned {
        let mut out = Assigned::default();
        each_expr(b, &mut |e: &Expr| match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
                if let E::Local(l, _) = place.kind {
                    out.whole.insert(l);
                }
                if let Some(root) = crate::body::places::place_root(place) {
                    out.rooted.insert(root);
                }
            }
            E::Closure(k) => {
                if let Some(Def::Fn(kf)) = &cx.defs[k.0 as usize] {
                    let inner = Assigned::of(cx, &kf.body.block);
                    for cap in &kf.captures {
                        if inner.whole.contains(&cap.inner) {
                            out.whole.insert(cap.outer);
                        }
                        if inner.rooted.contains(&cap.inner) {
                            out.rooted.insert(cap.outer);
                        }
                    }
                }
            }
            _ => {}
        });
        out
    }

    /// Is `l` assigned; with `projections` false only as a whole?
    pub(super) fn has(&self, l: LocalId, projections: bool) -> bool {
        if projections {
            self.rooted.contains(&l)
        } else {
            self.whole.contains(&l)
        }
    }
}

/// An enclosing function's body: what it assigns, and per local how often it is read, how
/// often called through (`f(...)`), and whether a closure created in it captures it.
pub(super) struct Facts {
    pub assigned: Assigned,
    uses: FxHashMap<LocalId, usize>,
    calls: FxHashMap<LocalId, usize>,
    captured: FxHashSet<LocalId>,
}

impl Facts {
    pub(super) fn of(cx: &Ctx, b: &Block) -> Facts {
        let mut facts = Facts {
            assigned: Assigned::of(cx, b),
            uses: FxHashMap::default(),
            calls: FxHashMap::default(),
            captured: FxHashSet::default(),
        };
        each_expr(b, &mut |e: &Expr| match &e.kind {
            E::Local(l, _) => *facts.uses.entry(*l).or_default() += 1,
            E::Call {
                callee: Callee::Indirect(callee),
                ..
            } => {
                if let E::Local(l, _) = callee.kind {
                    *facts.calls.entry(l).or_default() += 1;
                }
            }
            E::Closure(k) => {
                if let Some(Def::Fn(kf)) = &cx.defs[k.0 as usize] {
                    facts
                        .captured
                        .extend(kf.captures.iter().map(|cap| cap.outer));
                }
            }
            _ => {}
        });
        facts
    }

    /// Is every use of `local` the callee of a direct call, and no closure captures it?
    pub(super) fn only_called(&self, local: LocalId) -> bool {
        let count = |m: &FxHashMap<LocalId, usize>| m.get(&local).copied().unwrap_or(0);
        !self.captured.contains(&local) && count(&self.uses) == count(&self.calls)
    }
}
