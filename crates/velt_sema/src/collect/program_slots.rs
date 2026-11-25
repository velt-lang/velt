//! The vtable slots a program's overrides add to a finished unit's classes (`crate::units`,
//! #3684). A std method gets a slot only when something overrides it, so a program overriding a
//! std method without one (`Map.get`) needs a slot std's unit doesn't have, for its calls
//! through a std-typed receiver. The slot can't be written into std's frozen class: it is kept
//! beside it (`units::raised`'s `vtables`), read in its place by the app's passes
//! ([`Ctx::vtable_of`], [`Ctx::vslot`]) and written when the freeze lifts to build the program.
//!
//! Std's checked bodies keep their slot numbers, so a new slot goes after the longest vtable of
//! the class's std subtree, and every std class of that subtree gets it: its own method of that
//! key, or the one it inherits. The indices in between, slots of other branches of the subtree,
//! are filled with an entry the class already has; no call reaches a class through them. When an
//! override gives a parameter another default, the default-filling slots follow the method's
//! (`fill_defaults`), each std class of the subtree getting a fill method of its own where its
//! method differs from its base's, as `classes::fill_slots` lays them out in a whole-program
//! check.

use std::collections::BTreeMap;

use crate::ctx::def_table::crossing;
use crate::ctx::Ctx;
use crate::hir::DefId;

use super::classes::base_of_def;
use super::fill_defaults::{fill_key, fill_method, fill_positions, trailing_counts, Overrides};

/// Gives the finished unit's classes in `overridden` (origin class, method key) the slots they
/// lack, beside them, with the default-filling slots of a method an override in `overrides` gives
/// another default.
pub(super) fn add(
    cx: &mut Ctx,
    overridden: &std::collections::BTreeSet<(DefId, String)>,
    overrides: &Overrides,
) {
    let mut wanted: BTreeMap<DefId, Vec<String>> = BTreeMap::new();
    for (origin, key) in overridden {
        if !cx.is_frozen(*origin) || cx.vslot(*origin, key).is_some() {
            continue;
        }
        // A method the class gets as an interface's default has no method of its own to put in
        // the slot (a whole-program check makes a forwarder there): still counted.
        if own_method(cx, *origin, key).is_none() {
            crossing();
            continue;
        }
        wanted.entry(*origin).or_default().push(key.clone());
    }
    if wanted.is_empty() {
        return;
    }
    let frozen: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .take_while(|d| cx.is_frozen(*d))
        .filter(|d| cx.adt(*d).is_some_and(|a| a.decl.is_some()))
        .collect();
    let mut origins: Vec<(DefId, Vec<String>)> = wanted.into_iter().collect();
    // Bases first, so a subclass's own new slots go after its base's.
    origins.sort_by_key(|(o, _)| depth(cx, *o));
    for (origin, keys) in origins {
        add_slots(cx, origin, &keys, &frozen, overrides);
    }
}

/// A slot added to every class of a subtree: a method key's, or the default-filling one of a
/// key (`fill_defaults`) when a program override gives one of its parameters another default.
struct Added {
    /// The vtable key (`fill_key` for a fill slot).
    slot_key: String,
    /// For a fill slot: the method key, the parameters it fills and its trailing count.
    fill: Option<(String, Vec<usize>, Option<usize>)>,
}

/// The slots `keys` add to `origin`'s subtree, in order: each key's, then its fill slots, as
/// `classes::fill_slots` gives a class whose method an override gives another default.
fn added_slots(cx: &Ctx, origin: DefId, keys: &[String], overrides: &Overrides) -> Vec<Added> {
    let mut out = vec![];
    for k in keys {
        out.push(Added {
            slot_key: k.clone(),
            fill: None,
        });
        let Some(m) = own_method(cx, origin, k) else {
            continue;
        };
        let positions = fill_positions(cx, overrides, origin, k, m);
        if positions.is_empty() {
            continue;
        }
        let trailing = trailing_counts(cx, m, &positions);
        for t in std::iter::once(None).chain(trailing.into_iter().map(Some)) {
            out.push(Added {
                slot_key: fill_key(k, t),
                fill: Some((k.clone(), positions.clone(), t)),
            });
        }
    }
    out
}

/// Adds `keys` as slots of `origin` and of every std class under it.
fn add_slots(cx: &mut Ctx, origin: DefId, keys: &[String], frozen: &[DefId], o: &Overrides) {
    let mut subtree: Vec<DefId> = (frozen.iter().copied())
        .filter(|&c| c == origin || ancestors(cx, c).contains(&origin))
        .collect();
    subtree.sort_by_key(|&c| depth(cx, c));
    let first = (subtree.iter())
        .filter_map(|&c| cx.vtable_of(c).map(<[DefId]>::len))
        .max()
        .unwrap_or(0);
    let added = added_slots(cx, origin, keys, o);
    for c in subtree {
        let (mut entries, mut slots) = match cx.vtable_of(c).zip(cx.vslots_of(c)) {
            Some((e, s)) => (e.to_vec(), s.clone()),
            None => continue,
        };
        let parent = (c != origin).then(|| base_of_def(cx, c)).flatten();
        let mut new: Vec<DefId> = vec![];
        for a in &added {
            let def = match &a.fill {
                None => own_method(cx, c, &a.slot_key).or_else(|| entry(cx, parent, &a.slot_key)),
                Some((k, positions, t)) => {
                    // The method this class's slot `k` holds, computed just before.
                    let at = added
                        .iter()
                        .position(|b| b.fill.is_none() && b.slot_key == *k);
                    let m = new[at.expect("ICE: a fill slot's method slot")];
                    match entry(cx, parent, k) == Some(m) {
                        // Inherited unchanged: so is the fill entry.
                        true => entry(cx, parent, &a.slot_key),
                        false => Some(fill_method(cx, c, (m, k), positions, *t)),
                    }
                }
            };
            new.push(def.expect("ICE: a std class without the slot its base has"));
        }
        let pad = *entries.first().or(new.first()).expect("ICE: a slot added");
        entries.resize(first, pad);
        for (a, def) in added.iter().zip(new) {
            slots.insert(a.slot_key.clone(), entries.len() as u32);
            entries.push(def);
        }
        cx.unit_sums.raised.vtables.insert(c, (entries, slots));
    }
}

/// The entry of `parent`'s vtable at its slot `key` (beside it when the program added it).
fn entry(cx: &Ctx, parent: Option<DefId>, key: &str) -> Option<DefId> {
    let p = parent?;
    let slot = cx.vslot(p, key)?;
    cx.vtable_of(p)?.get(slot as usize).copied()
}

/// Class `c`'s own instance method `key`.
fn own_method(cx: &Ctx, c: DefId, key: &str) -> Option<DefId> {
    let m = cx.adt(c)?.methods.get(key)?;
    (!m.is_static).then_some(m.def)
}

/// The classes `c` extends, nearest first.
fn ancestors(cx: &Ctx, c: DefId) -> Vec<DefId> {
    let mut out = vec![];
    let mut at = base_of_def(cx, c);
    while let Some(b) = at {
        if out.contains(&b) {
            break;
        }
        out.push(b);
        at = base_of_def(cx, b);
    }
    out
}

fn depth(cx: &Ctx, c: DefId) -> usize {
    ancestors(cx, c).len()
}
