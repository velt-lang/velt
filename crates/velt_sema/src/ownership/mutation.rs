//! Mutation inference (docs/reference/memory.md "Mutation is inferred"): turns a body's
//! [`Evidence`](super::evidence::Evidence) into pass modes, and joins the modes of methods that
//! share a dispatch slot. Runs inside the ownership fixpoint (`super::infer`); modes only grow
//! (`Borrow` → `BorrowMut` → `Owned`), so the iteration terminates.
//!
//! - `this` whose contents the body modifies → `BorrowMut`.
//! - A non-Copy param whose contents the body modifies → `BorrowMut` (the caller sees the
//!   change, JS object semantics). Reassigned as a whole → `Owned` (a local rebinding: the
//!   callee owns its copy); when the body neither moves it nor modifies it before rebinding it
//!   (the first unconditional top-level `p = v`), callers pass a clone if they use the argument
//!   again ("soft", see [`super::soft`]) — the callee's changes are then invisible, as in JS. Functions with a fixed ABI
//!   (closures, overridden and interface methods) cannot own a borrowed param: reassigning one
//!   is reported by `super::finish`.
//! - Copy params are always passed by value; reassigning one is local.
//! - Closure params stay `Borrow` (a closure may be called with aliasing arguments, so they get
//!   no no-alias guarantee); their modification is recorded in `LocalDef::mutable`, like weak
//!   evidence (writes by function values, see `super::evidence`) for any param.
//! - Closure captures the closure modifies → `BorrowMut` (non-escaping closures) or `Owned`
//!   (escaping ones, which own their captures).
//! - Methods sharing a vtable slot (a base method and its overrides) get the join of their
//!   modes; an interface method's receiver / params are modified if any implementation (or the
//!   default) modifies them.

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{DefId, FnDef, LocalId, PassMode};

use super::evidence::{self, Evidence};
use super::patch::modes_of;

/// Apply the evidence of `d`'s body `f` (and the locals it moves from) to `d`'s modes.
/// Returns whether a mode (or a param's `LocalDef::mutable`) changed.
pub(super) fn apply(
    cx: &mut Ctx,
    d: DefId,
    f: &mut FnDef,
    ev: &Evidence,
    moved: &HashSet<LocalId>,
) -> bool {
    let ncap = f.captures.len();
    let mut changed = false;
    for p in &f.params {
        let l = p.local;
        let local = &mut f.body.locals[l.0 as usize];
        let written =
            ev.mutated.contains(&l) || ev.weak.contains(&l) || ev.reassigned.contains_key(&l);
        // A change for the fixpoint: callers passing this closure / function read `mutable`
        // (`super::patch`, callback rule), and so does an enclosing body's evidence.
        if written && !local.mutable {
            local.mutable = true;
            changed = true;
        }
    }
    changed |= captures(cx, d, f, ev);
    let info = cx.fn_info(d);
    let (kind, fixed, is_async) = (info.kind, info.fixed_modes, info.is_async);
    let has_this = info.this.is_some();
    if has_this && ev.mutated.contains(&f.params[0].local) {
        let t = cx.fn_info_mut(d).this.as_mut().expect("ICE: this");
        if t.mode == PassMode::Borrow {
            t.mode = PassMode::BorrowMut;
            changed = true;
        }
    }
    if kind == FnKind::Closure || is_async {
        return changed;
    }
    let first = ncap + usize::from(has_this);
    let mut soft = vec![];
    for (k, p) in f.params[first..].iter().enumerate() {
        let (mut mutated, moved) = (ev.mutated.contains(&p.local), moved.contains(&p.local));
        let reassigned = ev.reassigned.contains_key(&p.local);
        if mutated && reassigned {
            // `xs = []; xs.push(1)` only modifies the callee's own array.
            let before = evidence::modified_before_rebind(cx, &f.body.block, p.local);
            mutated = before.unwrap_or(true);
        }
        let sig = &mut cx.fn_info_mut(d).params[k];
        let new = match sig.mode {
            PassMode::Borrow | PassMode::BorrowMut if reassigned && !fixed => PassMode::Owned,
            PassMode::Borrow if mutated => PassMode::BorrowMut,
            m => m,
        };
        if new != sig.mode {
            sig.mode = new;
            changed = true;
        }
        if new == PassMode::Owned && reassigned && !mutated && !moved {
            soft.push(k);
        }
    }
    cx.fn_info_mut(d).soft_params = soft;
    changed
}

/// Capture modes of closure `f` from what its body does to the captured variables.
fn captures(cx: &mut Ctx, d: DefId, f: &mut FnDef, ev: &Evidence) -> bool {
    let escaping = cx.fn_info(d).escaping;
    let mut changed = false;
    for k in 0..f.captures.len() {
        let c = &mut f.captures[k];
        if !ev.mutated.contains(&c.inner) {
            continue;
        }
        let new = match c.mode {
            PassMode::Borrow | PassMode::Copy if !escaping => PassMode::BorrowMut,
            PassMode::Copy => PassMode::Owned,
            m => m,
        };
        f.body.locals[c.inner.0 as usize].mutable = true;
        if new != c.mode {
            c.mode = new;
            f.params[k].mode = new;
            changed = true;
        }
    }
    changed
}

/// Raise `Borrow` entries of `modes` to `BorrowMut` where `other` has `BorrowMut`.
fn join_into(modes: &mut [PassMode], other: &[PassMode]) {
    for (m, o) in modes.iter_mut().zip(other) {
        if *m == PassMode::Borrow && *o == PassMode::BorrowMut {
            *m = PassMode::BorrowMut;
        }
    }
}

/// Methods sharing a dispatch slot, found once per fixpoint (vtables and impls don't change
/// while modes are inferred), so `super::worklist` re-joins only the groups whose members
/// changed.
pub(super) struct Dispatch {
    /// Vtable groups first, then interface slots (a slot reads the joined method modes).
    groups: Vec<Group>,
}

enum Group {
    /// Methods in the same vtable slot of a class and its base classes.
    Vtable(Vec<DefId>),
    /// Interface `iface`'s method `slot`: the implementations and the default.
    Iface {
        iface: DefId,
        slot: usize,
        methods: Vec<DefId>,
    },
}

impl Dispatch {
    /// The dispatch groups of the whole program.
    pub(super) fn new(cx: &Ctx) -> Dispatch {
        let mut groups: Vec<Group> = vtable_groups(cx).into_iter().map(Group::Vtable).collect();
        groups.extend(iface_slots(cx));
        Dispatch { groups }
    }

    /// Number of groups.
    pub(super) fn len(&self) -> usize {
        self.groups.len()
    }

    /// Method → the groups it belongs to.
    pub(super) fn membership(&self) -> HashMap<DefId, Vec<usize>> {
        let mut out: HashMap<DefId, Vec<usize>> = HashMap::new();
        for (g, group) in self.groups.iter().enumerate() {
            let (Group::Vtable(methods) | Group::Iface { methods, .. }) = group;
            for &m in methods {
                out.entry(m).or_default().push(g);
            }
        }
        out
    }

    /// Method or interface → the groups whose joined modes a call through it reads.
    pub(super) fn slots(&self) -> HashMap<DefId, Vec<usize>> {
        let mut out = self.membership();
        for (g, group) in self.groups.iter().enumerate() {
            if let Group::Iface { iface, .. } = group {
                out.entry(*iface).or_default().push(g);
            }
        }
        out
    }

    /// The methods of group `g`.
    pub(super) fn members(&self, g: usize) -> &[DefId] {
        let (Group::Vtable(methods) | Group::Iface { methods, .. }) = &self.groups[g];
        methods
    }

    /// Join the modes of group `g`; pushes the defs (methods, interface) whose modes changed.
    pub(super) fn join(&self, cx: &mut Ctx, g: usize, changed: &mut Vec<DefId>) {
        match &self.groups[g] {
            Group::Vtable(methods) => join_group(cx, methods, changed),
            Group::Iface {
                iface,
                slot,
                methods,
            } => {
                if join_iface_slot(cx, *iface, *slot, methods) {
                    changed.push(*iface);
                }
            }
        }
    }
}

/// Methods in the same vtable slot of a class and its base classes (union-find over vtables).
fn vtable_groups(cx: &Ctx) -> Vec<Vec<DefId>> {
    let mut parent: HashMap<DefId, DefId> = HashMap::new();
    fn find(parent: &HashMap<DefId, DefId>, mut d: DefId) -> DefId {
        while let Some(&p) = parent.get(&d) {
            if p == d {
                break;
            }
            d = p;
        }
        d
    }
    for i in 0..cx.info.len() {
        let Some(a) = cx.adt(DefId(i as u32)) else {
            continue;
        };
        let Some(bvt) = a
            .base
            .and_then(|b| cx.class_of(b))
            .and_then(|(b, _)| cx.adt(b))
            .map(|b| &b.vtable)
        else {
            continue;
        };
        for (x, y) in a.vtable.iter().zip(bvt) {
            let (rx, ry) = (find(&parent, *x), find(&parent, *y));
            if rx != ry {
                parent.insert(rx, ry);
            }
        }
    }
    let mut members: Vec<DefId> = parent.keys().chain(parent.values()).copied().collect();
    members.sort_unstable();
    members.dedup();
    let mut groups: HashMap<DefId, Vec<DefId>> = HashMap::new();
    for m in members {
        groups.entry(find(&parent, m)).or_default().push(m);
    }
    let mut out: Vec<Vec<DefId>> = groups.into_values().collect();
    out.sort_unstable();
    out
}

/// Raise every method of `group` to the join of their modes (only `Borrow` → `BorrowMut`;
/// signatures that don't match were reported when the methods were collected). Pushes the
/// methods whose modes changed.
fn join_group(cx: &mut Ctx, group: &[DefId], changed: &mut Vec<DefId>) {
    let mut join = modes_of(cx, group[0]);
    for &m in &group[1..] {
        join_into(&mut join, &modes_of(cx, m));
    }
    for &m in group {
        let mut own = modes_of(cx, m);
        let before = own.clone();
        join_into(&mut own, &join);
        if own == before {
            continue;
        }
        changed.push(m);
        let f = cx.fn_info_mut(m);
        let mut it = own.into_iter();
        if let Some(t) = f.this.as_mut() {
            t.mode = it.next().expect("ICE: joined this");
        }
        for (p, j) in f.params.iter_mut().zip(it) {
            p.mode = j;
        }
    }
}

/// Every interface method slot with its implementations (in impl order) and default.
fn iface_slots(cx: &Ctx) -> Vec<Group> {
    let mut impls: HashMap<DefId, Vec<&[DefId]>> = HashMap::new();
    for im in &cx.impls {
        impls.entry(im.iface).or_default().push(&im.methods);
    }
    let mut out = vec![];
    for i in 0..cx.info.len() {
        let iface = DefId(i as u32);
        let Some(info) = cx.iface(iface) else {
            continue;
        };
        for (slot, m) in info.methods.iter().enumerate() {
            let methods = impls
                .get(&iface)
                .into_iter()
                .flatten()
                .filter_map(|methods| methods.get(slot).copied())
                .chain(m.default)
                .collect();
            out.push(Group::Iface {
                iface,
                slot,
                methods,
            });
        }
    }
    out
}

/// An interface method slot: the join over the default and every implementation. Returns
/// whether the slot's modes changed.
fn join_iface_slot(cx: &mut Ctx, iface: DefId, slot: usize, methods: &[DefId]) -> bool {
    let Some(m) = cx.iface(iface).map(|i| &i.methods[slot]) else {
        return false;
    };
    let mut join: Vec<PassMode> = std::iter::once(match m.mut_this {
        true => PassMode::BorrowMut,
        false => PassMode::Borrow,
    })
    .chain(m.params.iter().map(|p| p.mode))
    .collect();
    for &method in methods {
        join_into(&mut join, &modes_of(cx, method));
    }
    let DefInfo::Iface(info) = &mut cx.info[iface.0 as usize] else {
        unreachable!("ICE: iface")
    };
    let m = &mut info.methods[slot];
    let mut changed = false;
    let this_mut = join[0] == PassMode::BorrowMut;
    if this_mut != m.mut_this {
        m.mut_this = this_mut;
        changed = true;
    }
    for (p, j) in m.params.iter_mut().zip(&join[1..]) {
        if p.mode != *j {
            p.mode = *j;
            changed = true;
        }
    }
    changed
}
