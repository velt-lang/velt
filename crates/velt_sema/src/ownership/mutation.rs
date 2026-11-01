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

use rustc_hash::{FxHashMap, FxHashSet};

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
    moved: &FxHashSet<LocalId>,
) -> bool {
    let ncap = f.captures.len();
    let mut changed = false;
    for p in &f.params {
        let l = p.local;
        let local = &mut f.body.locals[l.0 as usize];
        // What a closure nested in this one changes counts too, for `modified_params`.
        let written = ev.mutated.contains(&l)
            || ev.weak.contains(&l)
            || ev.reassigned.contains_key(&l)
            || cx.frozen_reads && ev.modified.contains(&l);
        // A change for the fixpoint: callers passing this closure / function read `mutable`
        // (`super::patch`, callback rule), and so does an enclosing body's evidence.
        if written && !local.mutable {
            local.mutable = true;
            changed = true;
        }
    }
    changed |= captures(cx, d, f, ev);
    if cx.frozen_reads {
        let bits = (f.captures.iter().enumerate().take(64))
            .filter(|(_, c)| ev.modified.contains(&c.inner) || ev.mutated.contains(&c.inner))
            .fold(0u64, |b, (i, _)| b | 1 << i);
        let info = cx.fn_info_mut(d);
        if info.modified_captures | bits != info.modified_captures {
            info.modified_captures |= bits;
            changed = true;
        }
    }
    let info = cx.fn_info(d);
    // Async functions and generators keep their params (owned): nothing to infer.
    let (kind, fixed, is_async) = (
        info.kind,
        info.fixed_modes,
        info.is_async || info.is_generator,
    );
    let has_this = info.this.is_some();
    // `modified_params` only matters to a program that reads a `shared` (`evidence`).
    let frozen = cx.frozen_reads;
    let this_modified = has_this && ev.modified.contains(&f.params[0].local);
    if this_modified && cx.fn_info(d).modified_params & 1 == 0 {
        cx.fn_info_mut(d).modified_params |= 1;
        changed = true;
    }
    if has_this && ev.mutated.contains(&f.params[0].local) {
        let info = cx.fn_info_mut(d);
        if frozen && info.modified_params & 1 == 0 {
            info.modified_params |= 1;
            changed = true;
        }
        let t = info.this.as_mut().expect("ICE: this");
        if t.mode == PassMode::Borrow {
            t.mode = PassMode::BorrowMut;
            changed = true;
        }
    }
    let first = ncap + usize::from(has_this);
    // What the body changes of each param, whatever its mode (an `async` function's and a
    // generator's keep their arguments).
    for (k, p) in f.params[first..].iter().enumerate() {
        let bit = 1u64
            .checked_shl((k + usize::from(has_this)) as u32)
            .unwrap_or(0);
        let modified = ev.mutated.contains(&p.local) || ev.modified.contains(&p.local);
        let info = cx.fn_info_mut(d);
        if frozen && modified && info.modified_params & bit == 0 {
            info.modified_params |= bit;
            changed = true;
        }
    }
    if kind == FnKind::Closure || is_async {
        return changed;
    }
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
    /// Per vtable group, the join of its members' modes at its last join: a later join of a
    /// few changed members compares them with it rather than walking the whole group (a
    /// generated function's group holds every instance's body).
    joined: std::cell::RefCell<Vec<Option<Vec<PassMode>>>>,
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
        // A generated function and the generated bodies of its instances share one ABI: callers
        // are checked against the function, lowering calls the bodies (`crate::generated`).
        let mut generated: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
        for ((g, _), body) in &cx.gen.binding.bound {
            generated.entry(*g).or_insert_with(|| vec![*g]).push(*body);
        }
        let mut generated: Vec<Vec<DefId>> = generated.into_values().collect();
        generated.sort();
        groups.extend(generated.into_iter().map(Group::Vtable));
        groups.extend(iface_slots(cx));
        let joined = std::cell::RefCell::new(vec![None; groups.len()]);
        Dispatch { groups, joined }
    }

    /// Number of groups.
    pub(super) fn len(&self) -> usize {
        self.groups.len()
    }

    /// Method → the groups it belongs to.
    pub(super) fn membership(&self) -> FxHashMap<DefId, Vec<usize>> {
        let mut out: FxHashMap<DefId, Vec<usize>> = FxHashMap::default();
        for (g, group) in self.groups.iter().enumerate() {
            let (Group::Vtable(methods) | Group::Iface { methods, .. }) = group;
            for &m in methods {
                out.entry(m).or_default().push(g);
            }
        }
        out
    }

    /// Method or interface → the groups whose joined modes a call through it reads.
    pub(super) fn slots(&self) -> FxHashMap<DefId, Vec<usize>> {
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

    /// Join the modes of group `g`, of which `members` changed since its last join (`None`:
    /// any may have); pushes the defs (methods, interface) whose modes changed.
    pub(super) fn join(
        &self,
        cx: &mut Ctx,
        g: usize,
        members: Option<&[DefId]>,
        changed: &mut Vec<DefId>,
    ) {
        match &self.groups[g] {
            Group::Vtable(methods) => {
                let mut joined = self.joined.borrow_mut();
                join_group(cx, methods, members, &mut joined[g], changed)
            }
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
    let mut parent: FxHashMap<DefId, DefId> = FxHashMap::default();
    fn find(parent: &FxHashMap<DefId, DefId>, mut d: DefId) -> DefId {
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
    let mut groups: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
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
///
/// `joined`: the join at the last call, kept for the next. With it and the `members` that
/// changed since, only those are joined in, and only they are raised unless the join grew
/// (modes only grow, so every other member is at the old join already); the same modes as
/// joining the whole group.
fn join_group(
    cx: &mut Ctx,
    group: &[DefId],
    members: Option<&[DefId]>,
    joined: &mut Option<Vec<PassMode>>,
    changed: &mut Vec<DefId>,
) {
    let (join, raise) = group_join(group, members, joined.take(), |m| modes_of(cx, m));
    *joined = Some(join.clone());
    for &m in raise {
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

/// The join of `group`'s modes and the members to raise to it (see [`join_group`]).
fn group_join<'g>(
    group: &'g [DefId],
    members: Option<&'g [DefId]>,
    joined: Option<Vec<PassMode>>,
    mut modes_of: impl FnMut(DefId) -> Vec<PassMode>,
) -> (Vec<PassMode>, &'g [DefId]) {
    // The join starts from the first member's modes, so a change of that member joins all.
    let members = members.filter(|ms| !ms.contains(&group[0]));
    match (joined, members) {
        (Some(mut join), Some(members)) => {
            let before = join.clone();
            for &m in members {
                join_into(&mut join, &modes_of(m));
            }
            let raise = if join == before { members } else { group };
            (join, raise)
        }
        _ => {
            let mut join = modes_of(group[0]);
            for &m in &group[1..] {
                join_into(&mut join, &modes_of(m));
            }
            (join, group)
        }
    }
}

/// Every interface method slot with its implementations (in impl order) and default.
fn iface_slots(cx: &Ctx) -> Vec<Group> {
    let mut impls: FxHashMap<DefId, Vec<&[DefId]>> = FxHashMap::default();
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
        let mut modes = modes_of(cx, method);
        // A generic closure implementing a generic function type (`crate::generic_fns`): its
        // environment stands for `this`.
        if cx.fn_info(method).kind == crate::defs::FnKind::Closure {
            modes.insert(0, PassMode::Borrow);
        }
        join_into(&mut join, &modes);
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

#[cfg(test)]
mod tests {
    use super::*;
    use PassMode::{Borrow, BorrowMut};

    #[test]
    fn a_join_of_changed_members_raises_the_group_only_when_it_grows() {
        let group = [DefId(1), DefId(2), DefId(3)];
        let modes = |mutated: &'static [u32]| {
            move |d: DefId| {
                vec![if mutated.contains(&d.0) {
                    BorrowMut
                } else {
                    Borrow
                }]
            }
        };
        let (join, raise) = group_join(&group, None, None, modes(&[]));
        assert_eq!((join.clone(), raise), (vec![Borrow], &group[..]));
        // One member turns `BorrowMut` after the group was joined: every member is raised.
        let changed = [DefId(2)];
        let (join, raise) = group_join(&group, Some(&changed), Some(join), modes(&[2]));
        assert_eq!((join.clone(), raise), (vec![BorrowMut], &group[..]));
        // Another one does too, but the join stays: only that member is looked at.
        let changed = [DefId(3)];
        let (join, raise) = group_join(&group, Some(&changed), Some(join), modes(&[2, 3]));
        assert_eq!((join, raise), (vec![BorrowMut], &changed[..]));
        // The first member changed: the group is joined whole.
        let changed = [DefId(1)];
        let (_, raise) = group_join(&group, Some(&changed), Some(vec![Borrow]), modes(&[1]));
        assert_eq!(raise, &group[..]);
    }
}
