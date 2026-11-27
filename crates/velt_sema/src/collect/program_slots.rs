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
//! check. A method std gave a slot (because std overrides it) gets only the fill slots it lacks
//! (`missing_fills`).

use std::collections::BTreeMap;

use crate::ctx::def_table::crossing;
use crate::ctx::Ctx;
use crate::hir::DefId;

use super::classes::base_of_def;
use super::fill_defaults::{
    fill_key, fill_method, fill_positions, trailing_counts, Overrides, FILL,
};

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
        if !cx.is_frozen(*origin) {
            continue;
        }
        // A method std gave a slot (std overrides it) may still lack the default-filling slots
        // of the program's other default (std's overrides keep the method's).
        if cx.vslot(*origin, key).is_some() {
            if !missing_fills(cx, *origin, key, overrides).is_empty() {
                wanted.entry(*origin).or_default().push(key.clone());
            }
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
        let added = keys.into_iter().map(|k| (origin, k));
        cx.unit_sums.raised.added.extend(added);
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

/// The slots `keys` add to `origin`'s subtree, in order: each key's when std gave it none, then
/// the fill slots it lacks, as `classes::fill_slots` gives a class whose method an override
/// gives another default.
fn added_slots(cx: &Ctx, origin: DefId, keys: &[String], overrides: &Overrides) -> Vec<Added> {
    let mut out = vec![];
    for k in keys {
        if cx.vslot(origin, k).is_none() {
            out.push(Added {
                slot_key: k.clone(),
                fill: None,
            });
        }
        out.extend(missing_fills(cx, origin, k, overrides));
    }
    out
}

/// The default-filling slots method `key` of `origin` (introducing it) lacks for the defaults
/// the program's overrides in `overrides` give it: none when no override changes one. Std's
/// own fill slots (when std's overrides change a default too) are kept: the general one is
/// added again when the program's take more parameters (`add_slots` keeps std's under another
/// key, for std's checked calls and their entries in the program's classes), and a trailing one
/// only for a count std has none for.
fn missing_fills(cx: &Ctx, origin: DefId, key: &str, overrides: &Overrides) -> Vec<Added> {
    let Some(m) = own_method(cx, origin, key) else {
        return vec![];
    };
    let mut positions = fill_positions(cx, overrides, origin, key, m);
    if positions.is_empty() {
        return vec![];
    }
    let general = fill_key(key, None);
    let std_positions = (cx.vslot(origin, &general))
        .and_then(|s| cx.vtable_of(origin)?.get(s as usize).copied())
        .and_then(|e| cx.fills.get(&e))
        .map(|f| f.positions.clone());
    let mut out = vec![];
    let mut add = |t: Option<usize>, positions: &[usize]| {
        out.push(Added {
            slot_key: fill_key(key, t),
            fill: Some((key.to_string(), positions.to_vec(), t)),
        })
    };
    match std_positions {
        Some(had) if positions.iter().all(|p| had.contains(p)) => positions = had,
        Some(had) => {
            positions.extend(had);
            positions.sort_unstable();
            positions.dedup();
            add(None, &positions);
        }
        None => add(None, &positions),
    }
    for t in trailing_counts(cx, m, &positions) {
        if cx.vslot(origin, &fill_key(key, Some(t))).is_none() {
            add(Some(t), &positions);
        }
    }
    out
}

/// The key std's own general fill slot of a method keeps when the program adds a wider one
/// (`missing_fills`): a fill slot still (`classes::fill_slots` gives a program class an entry
/// of its own there), no call names it.
fn std_fill_key(key: &str) -> String {
    format!("{FILL}<std>{}", key.strip_prefix(FILL).unwrap_or(key))
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
                    // The method this class's slot `k` holds: computed just before, or std's.
                    let at = added
                        .iter()
                        .position(|b| b.fill.is_none() && b.slot_key == *k);
                    let m = match at {
                        Some(at) => new[at],
                        None => entry(cx, Some(c), k).expect("ICE: a fill slot's method slot"),
                    };
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
            // Std's general fill slot, which a wider one replaces for the program's calls.
            if let Some(old) = slots.get(&a.slot_key).copied() {
                slots.insert(std_fill_key(&a.slot_key), old);
            }
            slots.insert(a.slot_key.clone(), entries.len() as u32);
            entries.push(def);
            if a.fill.is_some() {
                let fill = (c, a.slot_key.clone());
                cx.unit_sums.raised.added_fills.insert(fill);
            }
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
