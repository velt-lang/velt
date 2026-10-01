//! Swap or restart: compares two versions' [`Facts`] per function key (docs/internals/design/hot-reload.md,
//! "Which edits hot-swap").
//!
//! A restart is needed when live state could meet code built for another shape of it:
//! - a layout live data may have changed (a struct/class gained or lost a field, a closure's
//!   captures changed): some layout name lost a structure *and* gained another;
//! - a function's signature changed (frames on the stack would call it with the old ABI);
//! - `main` changed (its synchronous part already ran and will not run again);
//! - a function whose address live data may hold (closure, function value, vtable slot) was
//!   removed, or closures were inserted before existing ones (their numbers shifted);
//! - an ambiguous key changed.
//!
//! Otherwise the version is swapped in. The functions to compile are the changed and new ones,
//! plus every function that refers to a pinned function being recompiled (a parent poll
//! embedding a changed child's state, an async function's entry creating its new state, a handler
//! descriptor), transitively.

use std::collections::{HashMap, HashSet};

use super::facts::Facts;
use super::roles;

/// What to do with a new version.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Restart the program; the reason, for the user.
    Restart(String),
    /// Swap in these functions (keys of the new version).
    Swap(Vec<String>),
}

/// Compare the running version `old` with `new`.
pub(crate) fn classify(old: &Facts, new: &Facts) -> Decision {
    let checks = [
        ambiguous_change,
        layout_change,
        signature_change,
        main_change,
        removed_entry_point,
        shifted_closures,
    ];
    for check in checks {
        if let Some(reason) = check(old, new) {
            return Decision::Restart(reason);
        }
    }
    Decision::Swap(to_compile(old, new))
}

fn changed(old: &Facts, new: &Facts, key: &str) -> bool {
    match (old.funcs.get(key), new.funcs.get(key)) {
        (Some(a), Some(b)) => a.fingerprint != b.fingerprint,
        _ => true,
    }
}

fn ambiguous_change(old: &Facts, new: &Facts) -> Option<String> {
    let mut keys: Vec<&String> = new
        .funcs
        .keys()
        .chain(old.funcs.keys())
        .filter(|k| roles::is_ambiguous(k) && changed(old, new, k))
        .collect();
    keys.sort();
    keys.first()
        .map(|k| format!("`{}` has an ambiguous name", roles::display(k)))
}

fn layout_change(old: &Facts, new: &Facts) -> Option<String> {
    let mut names: Vec<&String> = old.layouts.keys().collect();
    names.sort();
    for name in names {
        let (Some(before), Some(after)) = (old.layouts.get(name), new.layouts.get(name)) else {
            continue;
        };
        let lost = before.keys().find(|s| !after.contains_key(*s));
        let gained = after.keys().find(|s| !before.contains_key(*s));
        if let (Some(lost), Some(gained)) = (lost, gained) {
            return Some(describe_layout_change(name, before[lost], after[gained]));
        }
    }
    None
}

/// `Point gained a field`, `the captures of main::{closure#0} changed`, … (from the field
/// counts of a lost and a gained layout).
fn describe_layout_change(name: &str, before: usize, after: usize) -> String {
    if let Some(closure) = name.strip_suffix(" env") {
        return format!("the captures of {closure} changed");
    }
    let shown = name.strip_suffix(" object").unwrap_or(name);
    match after.cmp(&before) {
        std::cmp::Ordering::Greater => format!("{shown} gained a field"),
        std::cmp::Ordering::Less => format!("{shown} lost a field"),
        std::cmp::Ordering::Equal => format!("the layout of {shown} changed"),
    }
}

fn signature_change(old: &Facts, new: &Facts) -> Option<String> {
    let mut keys: Vec<&String> = new.funcs.keys().collect();
    keys.sort();
    keys.into_iter()
        .find(|k| {
            old.funcs
                .get(*k)
                .is_some_and(|f| f.signature != new.funcs[*k].signature)
        })
        .map(|k| format!("the signature of {} changed", roles::display(k)))
}

fn main_change(old: &Facts, new: &Facts) -> Option<String> {
    let mut keys: Vec<&String> = old.main.union(&new.main).collect();
    keys.sort();
    keys.into_iter()
        .find(|k| changed(old, new, k))
        .map(|_| "main changed (it already ran)".to_string())
}

fn removed_entry_point(old: &Facts, new: &Facts) -> Option<String> {
    let mut keys: Vec<&String> = old
        .address_taken
        .iter()
        .filter(|k| !roles::is_pinned(k) && !new.funcs.contains_key(*k))
        .collect();
    keys.sort();
    keys.first().map(|k| {
        format!(
            "{} was removed, but live values may still call it",
            roles::display(k)
        )
    })
}

/// Closures are numbered in source order: a new closure before existing ones renumbers them,
/// and a live closure would then run another closure's code. Detected as: a parent whose
/// closure count changed while one of the closures both versions have changed.
fn shifted_closures(old: &Facts, new: &Facts) -> Option<String> {
    let count = |facts: &Facts| {
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        for key in facts.funcs.keys() {
            if let Some((parent, _)) = roles::closure_slot(key) {
                parents.entry(parent).or_default().push(key.clone());
            }
        }
        parents
    };
    let (before, after) = (count(old), count(new));
    let mut parents: Vec<&String> = before.keys().collect();
    parents.sort();
    for parent in parents {
        let Some(now) = after.get(parent) else {
            continue;
        };
        let was = &before[parent];
        if was.len() != now.len()
            && was
                .iter()
                .any(|k| new.funcs.contains_key(k) && changed(old, new, k))
        {
            return Some(format!("closures in {parent} were added or reordered"));
        }
    }
    None
}

/// Changed and new functions plus, transitively, the functions referring to a pinned function
/// that is recompiled.
fn to_compile(old: &Facts, new: &Facts) -> Vec<String> {
    let mut dirty: HashSet<&String> = new.funcs.keys().filter(|k| changed(old, new, k)).collect();
    loop {
        let more: Vec<&String> = new
            .funcs
            .iter()
            .filter(|(k, f)| !dirty.contains(k) && f.pinned_refs.iter().any(|r| dirty.contains(r)))
            .map(|(k, _)| k)
            .collect();
        if more.is_empty() {
            break;
        }
        dirty.extend(more);
    }
    let mut keys: Vec<String> = dirty.into_iter().cloned().collect();
    keys.sort();
    keys
}
