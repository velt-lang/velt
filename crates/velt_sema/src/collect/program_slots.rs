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
//! are filled with an entry the class already has; no call reaches a class through them.

use std::collections::BTreeMap;

use crate::ctx::def_table::crossing;
use crate::ctx::Ctx;
use crate::hir::DefId;

use super::classes::base_of_def;

/// Gives the finished unit's classes in `overridden` (origin class, method key) the slots they
/// lack, beside them.
pub(super) fn add(cx: &mut Ctx, overridden: &std::collections::BTreeSet<(DefId, String)>) {
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
        add_slots(cx, origin, &keys, &frozen);
    }
}

/// Adds `keys` as slots of `origin` and of every std class under it.
fn add_slots(cx: &mut Ctx, origin: DefId, keys: &[String], frozen: &[DefId]) {
    let mut subtree: Vec<DefId> = (frozen.iter().copied())
        .filter(|&c| c == origin || ancestors(cx, c).contains(&origin))
        .collect();
    subtree.sort_by_key(|&c| depth(cx, c));
    let first = (subtree.iter())
        .filter_map(|&c| cx.vtable_of(c).map(<[DefId]>::len))
        .max()
        .unwrap_or(0);
    for c in subtree {
        let (mut entries, mut slots) = match cx.vtable_of(c).zip(cx.vslots_of(c)) {
            Some((e, s)) => (e.to_vec(), s.clone()),
            None => continue,
        };
        let parent = (c != origin).then(|| base_of_def(cx, c)).flatten();
        let new: Vec<DefId> = (keys.iter())
            .filter_map(|k| {
                own_method(cx, c, k).or_else(|| {
                    let p = parent?;
                    let slot = cx.vslot(p, k)?;
                    cx.vtable_of(p)?.get(slot as usize).copied()
                })
            })
            .collect();
        let Some(&pad) = entries.first().or(new.first()) else {
            continue;
        };
        if new.len() != keys.len() {
            continue;
        }
        entries.resize(first, pad);
        for (k, def) in keys.iter().zip(new) {
            slots.insert(k.clone(), entries.len() as u32);
            entries.push(def);
        }
        cx.unit_sums.raised.vtables.insert(c, (entries, slots));
    }
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
