//! The final error type of every function: a fixpoint over the call graph (error types only
//! grow, and there are finitely many thrown types, so it terminates). Functions with a written
//! (or context-fixed) `throws` keep it; dispatch groups get their bound or the union of what
//! their members throw.

use rustc_hash::FxHashMap;

use velt_common::Span;

use super::InitsSeen;
use crate::ctx::Ctx;
use crate::defs::{BodyState, ThrowSrc};
use crate::hir::{Def, DefId, TyId, TyKind};

/// Fill `FnInfo::throws` of every checked function, then check written clauses and the error
/// types sema committed to while checking bodies.
pub(crate) fn infer_all(cx: &mut Ctx) {
    // Built again with everything checking made: the implementations of object literals'
    // interfaces and the conversions between interfaces (`groups.rs`) are made after a first
    // use while checking.
    cx.groups = None;
    let groups = cx.throw_groups().clone();
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for &d in &fns {
        let decl = cx.fn_info(d).declared_throws.map(|t| t.ty);
        let t = decl.flatten();
        cx.fn_info_mut(d).throws = cx.canon_error(t);
    }
    for g in &groups.list {
        if let Some(b) = &g.bound {
            for &m in &g.members {
                let t = super::groups::member_bound(cx, b, m);
                cx.fn_info_mut(m).throws = t;
            }
        }
    }
    let mut guarded = Guarded::new(cx, &fns);
    let mut work = Worklist::new(cx, &fns, &groups);
    let mut round = 0;
    loop {
        // The functions to evaluate: all of them at first, and all again after the stored
        // closures grew a site's set; otherwise the callers of what changed (queued as it
        // changed) and the ones that read more than their callees.
        if round == 0 {
            work.queue_all();
        } else {
            work.queue_volatile();
        }
        round += 1;
        let mut changed = work.settle(cx, &mut guarded);
        for g in groups.list.iter().filter(|g| g.bound.is_none()) {
            let mut acc = None;
            for &m in &g.members {
                let t = member_final(cx, m);
                acc = cx.join_errors(acc, t);
            }
            for &m in &g.members {
                if set_throws(cx, m, acc) {
                    work.queue_callers(m);
                    changed = true;
                }
            }
        }
        if super::checks::seed_stored(cx, &fns) {
            round = 0;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    super::checks::check_all(cx, &fns, &groups);
    cx.record_recursion_hints(&fns);
}

/// The fixpoint's functions whose error type is inferred (no written clause, no dispatch
/// group), evaluated as a worklist: a function is evaluated again when a callee's error type
/// changed, rather than every function in every round.
struct Worklist {
    open: Vec<DefId>,
    /// The functions (indexes into `open`) that call each function directly.
    callers: FxHashMap<DefId, Vec<u32>>,
    /// The functions whose sources read more than their callees' error types (field
    /// defaults, interface slots): evaluated again in every round after a change.
    volatile: Vec<u32>,
    queued: Vec<bool>,
    queue: std::collections::VecDeque<u32>,
}

impl Worklist {
    fn new(cx: &Ctx, fns: &[DefId], groups: &super::groups::Groups) -> Worklist {
        let open: Vec<DefId> = (fns.iter().copied())
            .filter(|&d| cx.fn_info(d).declared_throws.is_none() && groups.group_of(d).is_none())
            .collect();
        let mut callers: FxHashMap<DefId, Vec<u32>> = FxHashMap::default();
        let mut volatile = vec![];
        for (i, &d) in open.iter().enumerate() {
            let i = i as u32;
            let mut reads_more = false;
            for s in &cx.fn_info(d).throw_srcs {
                match s {
                    ThrowSrc::Call(g, _, _) => {
                        let v = callers.entry(*g).or_default();
                        if v.last() != Some(&i) {
                            v.push(i);
                        }
                    }
                    ThrowSrc::Defaults(..) | ThrowSrc::Slot { .. } => reads_more = true,
                    ThrowSrc::Direct(..) | ThrowSrc::Indirect(..) => {}
                }
            }
            if reads_more {
                volatile.push(i);
            }
        }
        let n = open.len();
        Worklist {
            open,
            callers,
            volatile,
            queued: vec![false; n],
            queue: Default::default(),
        }
    }

    fn push(&mut self, i: u32) {
        if !std::mem::replace(&mut self.queued[i as usize], true) {
            self.queue.push_back(i);
        }
    }

    fn queue_all(&mut self) {
        (0..self.open.len() as u32).for_each(|i| self.push(i));
    }

    fn queue_volatile(&mut self) {
        for k in 0..self.volatile.len() {
            self.push(self.volatile[k]);
        }
    }

    /// Queue the callers of `d`, whose error type changed.
    fn queue_callers(&mut self, d: DefId) {
        let Some(cs) = self.callers.get(&d) else {
            return;
        };
        for k in 0..cs.len() {
            let c = self.callers[&d][k];
            self.push(c);
        }
    }

    /// Evaluate the queued functions until none is left; true when an error type changed.
    fn settle(&mut self, cx: &mut Ctx, guarded: &mut Guarded) -> bool {
        let mut changed = false;
        while let Some(i) = self.queue.pop_front() {
            self.queued[i as usize] = false;
            let d = self.open[i as usize];
            let (t, moved) = match guarded.ranges.contains_key(&d) {
                true => guarded.own_final(cx, d),
                false => (own_final(cx, d), false),
            };
            if set_throws(cx, d, t) | moved {
                changed = true;
                self.queue_callers(d);
            }
        }
        changed
    }
}

/// Error inference for generic functions with branches a type predicate on a type parameter's
/// value guards (`if (isPrimitive(x))`, #1251): such a branch is not instantiated where the
/// parameter is an object type, so neither is what it throws. Each error type member of such a
/// function carries the bits of the parameters whose guards it lies under, carried through
/// calls as the requirement passes carry theirs (`inst_sites::carry_guard`); a member reaching a
/// call that makes a guarded parameter an object type is dropped. Without this a function that
/// calls itself as `f({ value: x })` in such a branch would grow its error type without end.
#[derive(Default)]
struct Guarded {
    /// The guarded `then` blocks of each such function, with their parameters' bits.
    ranges: FxHashMap<DefId, Vec<(Span, u64)>>,
    /// The members of each such function's error type, with their guard bits.
    members: FxHashMap<DefId, Vec<(TyId, u64)>>,
}

impl Guarded {
    fn new(cx: &mut Ctx, fns: &[DefId]) -> Guarded {
        let mut g = Guarded::default();
        if !cx.primitive_guards {
            return g;
        }
        struct Ranges<'a> {
            ty: &'a crate::types::Types,
            out: Vec<(Span, u64)>,
        }
        impl crate::visit::VisitMut for Ranges<'_> {
            fn stmt(&mut self, s: &mut crate::hir::Stmt) {
                let crate::hir::StmtKind::If { cond, then, .. } = &s.kind else {
                    return;
                };
                let p = crate::visit::guard_param(cond).and_then(|t| match self.ty.kind(t) {
                    TyKind::Param(p) => Some(*p),
                    _ => None,
                });
                if let Some(bit) = p.and_then(|p| 1u64.checked_shl(p)) {
                    self.out.push((then.span, bit));
                }
            }
        }
        for &d in fns {
            let Some(Some(Def::Fn(f))) = cx.defs.get_mut(d.0 as usize) else {
                continue;
            };
            if f.generics == 0 {
                continue;
            }
            let mut r = Ranges {
                ty: &cx.ty,
                out: vec![],
            };
            crate::visit::block(&mut f.body.block, &mut r);
            if !r.out.is_empty() {
                g.ranges.insert(d, r.out);
            }
        }
        g
    }

    /// The guard bits of a source at `span` in `d`.
    fn site(&self, d: DefId, span: Span) -> u64 {
        self.ranges.get(&d).map_or(0, |rs| {
            rs.iter()
                .filter(|(r, _)| r.file == span.file && r.lo <= span.lo && span.hi <= r.hi)
                .fold(0, |acc, (_, b)| acc | b)
        })
    }

    /// What `d`'s body throws ([`own_final`]), dropping what guarded branches throw in the
    /// instances that don't have them; with whether its members or their bits changed.
    fn own_final(&mut self, cx: &mut Ctx, d: DefId) -> (Option<TyId>, bool) {
        let srcs = cx.fn_info(d).throw_srcs.clone();
        let mut out: Vec<(TyId, u64)> = vec![];
        for s in &srcs {
            let site = self.site(d, s.span());
            match s {
                ThrowSrc::Call(g, targs, _) if self.ranges.contains_key(g) => {
                    let callee = self.members.get(g).cloned().unwrap_or_default();
                    for (m, need) in callee {
                        let Some(bits) = crate::inst_sites::carry_guard(cx, need, targs, site)
                        else {
                            continue;
                        };
                        let t = cx.subst(m, targs);
                        for x in cx.error_members(t) {
                            push_member(&mut out, x, bits);
                        }
                    }
                }
                _ => {
                    if let Some(t) = src_final(cx, s) {
                        for x in cx.error_members(t) {
                            push_member(&mut out, x, site);
                        }
                    }
                }
            }
        }
        let all: Vec<TyId> = out.iter().map(|(m, _)| *m).collect();
        let moved = self.members.get(&d) != Some(&out);
        self.members.insert(d, out);
        (cx.error_union(&all), moved)
    }
}

/// Adds member `m` under guard bits `bits` (a member under fewer guards subsumes the others).
fn push_member(out: &mut Vec<(TyId, u64)>, m: TyId, bits: u64) {
    match out.iter_mut().find(|(x, _)| *x == m) {
        Some((_, b)) => *b &= bits,
        None => out.push((m, bits)),
    }
}

fn set_throws(cx: &mut Ctx, d: DefId, t: Option<TyId>) -> bool {
    if cx.fn_info(d).throws == t {
        return false;
    }
    cx.fn_info_mut(d).throws = t;
    true
}

/// What a group member contributes: its written clause, else what its body throws.
pub(super) fn member_final(cx: &mut Ctx, d: DefId) -> Option<TyId> {
    match cx.fn_info(d).declared_throws {
        Some(decl) => cx.canon_error(decl.ty),
        None => own_final(cx, d),
    }
}

/// What `d`'s body throws, with the current error types of its callees.
pub(super) fn own_final(cx: &mut Ctx, d: DefId) -> Option<TyId> {
    let srcs = cx.fn_info(d).throw_srcs.clone();
    srcs_final(cx, &srcs)
}

pub(super) fn srcs_final(cx: &mut Ctx, srcs: &[ThrowSrc]) -> Option<TyId> {
    // The union of what the sources throw, made once: many sources throw the same set.
    let mut all: Vec<TyId> = vec![];
    for s in srcs {
        if let Some(t) = src_final(cx, s) {
            if !all.contains(&t) {
                all.push(t);
            }
        }
    }
    cx.error_union(&all)
}

/// What one throw source throws, with the current error types.
pub(super) fn src_final(cx: &mut Ctx, s: &ThrowSrc) -> Option<TyId> {
    src_final_in(cx, s, &mut InitsSeen::default())
}

/// `src_final`, where the field defaults of the classes in `visited` are already counted.
fn src_final_in(cx: &mut Ctx, s: &ThrowSrc, visited: &mut InitsSeen) -> Option<TyId> {
    let (t, args) = match s {
        ThrowSrc::Direct(t, _) => return cx.canon_error(Some(*t)),
        ThrowSrc::Indirect(t, site, _) => return cx.indirect_error(*t, *site),
        ThrowSrc::Defaults(d, args, span) => {
            if !visited.enter(&cx.ty, *d, args) {
                return None;
            }
            let mut acc = None;
            for s in super::defaults_srcs(cx, *d, args, *span) {
                let t = src_final_in(cx, &s, visited);
                acc = cx.join_errors(acc, t);
            }
            visited.leave();
            return acc;
        }
        ThrowSrc::Call(g, targs, _) => (cx.try_fn(*g).and_then(|f| f.throws), targs.clone()),
        ThrowSrc::Slot {
            iface, slot, args, ..
        } if super::slot_clause(cx, *iface, *slot).is_some() => {
            let t = super::slot_clause(cx, *iface, *slot).flatten();
            (t, args.clone())
        }
        ThrowSrc::Slot {
            iface, slot, args, ..
        } => (slot_group_final(cx, *iface, *slot), args.clone()),
    };
    let t = t.map(|t| cx.subst(t, &args));
    cx.canon_error(t)
}

/// What a call through slot `slot` of `iface`, written without a `throws` clause, throws (in
/// the interface's terms): its group's error type.
pub(super) fn slot_group_final(cx: &mut Ctx, iface: DefId, slot: u32) -> Option<TyId> {
    let groups = cx.throw_groups();
    let member = groups
        .slot_group(iface, slot)
        .and_then(|g| groups.list[g].members.first().copied());
    let t = match member {
        Some(m) => cx.fn_info(m).throws,
        None => bound_of_slot(cx, iface, slot),
    };
    // A generic group error type is reported once, where it is inferred (checks.rs).
    t.filter(|t| !cx.mentions_params(*t))
}

/// A slot without any implementation: its written clause (if any).
fn bound_of_slot(cx: &mut Ctx, iface: DefId, slot: u32) -> Option<TyId> {
    let groups = cx.throw_groups();
    let g = groups.slot_group(iface, slot)?;
    groups.list[g].bound.as_ref().and_then(|b| b.decl.ty)
}
