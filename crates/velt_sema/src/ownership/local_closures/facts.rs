//! What the parent module asks of an enclosing function's body for each of its closures, found
//! in one walk of the body: which locals it assigns and how it uses them. Asking by walking the
//! body once per closure (and per capture) cost the square of a function that creates many
//! closures.

use std::cell::OnceCell;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{Block, Callee, Def, DefId, Expr, ExprKind as E, LocalId};

use super::{each_expr, sites};

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
    /// The role of each local of the function.
    pub kinds: Vec<LocalKind>,
    /// Its borrowing pattern bindings (`for...of` elements, destructured names).
    pub aliases: FxHashSet<LocalId>,
    /// The locals holding the closures being decided.
    held: FxHashSet<LocalId>,
    /// Per such local: the variables held or mentioned at its calls ([`sites`]), walked for
    /// when first asked.
    at_calls: OnceCell<FxHashMap<LocalId, FxHashSet<LocalId>>>,
}

impl Facts {
    /// The facts of `d`'s body `b`, holding the closures in the locals `held`.
    pub(super) fn of(cx: &Ctx, d: DefId, b: &Block, held: FxHashSet<LocalId>) -> Facts {
        let kinds = cx.fn_info(d).local_kinds.clone();
        let aliases = (kinds.iter().enumerate())
            .filter(|(_, k)| matches!(k, LocalKind::Bind | LocalKind::Elem))
            .map(|(i, _)| LocalId(i as u32))
            .collect();
        let mut facts = Facts {
            assigned: Assigned::of(cx, b),
            uses: FxHashMap::default(),
            calls: FxHashMap::default(),
            captured: FxHashSet::default(),
            kinds,
            aliases,
            held,
            at_calls: OnceCell::new(),
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

    /// Per closure local: the variables held or mentioned at its calls in the body `b`.
    pub(super) fn at_calls(&self, cx: &Ctx, b: &Block) -> &FxHashMap<LocalId, FxHashSet<LocalId>> {
        self.at_calls.get_or_init(|| {
            let mut s = sites::Sites::new(cx, &self.held, &self.kinds);
            s.block(b);
            crate::check_work::add_local_closure_steps(s.steps);
            s.at_calls
        })
    }

    /// Is every use of `local` the callee of a direct call, and no closure captures it?
    pub(super) fn only_called(&self, local: LocalId) -> bool {
        let count = |m: &FxHashMap<LocalId, usize>| m.get(&local).copied().unwrap_or(0);
        !self.captured.contains(&local) && count(&self.uses) == count(&self.calls)
    }
}
