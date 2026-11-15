//! A table indexed by `DefId` whose first entries can be frozen (#2943 M1, units in sema).
//!
//! Units are checked in dependency order, so a finished unit's definitions are a prefix of the
//! table: `frozen_below` marks it. Reads are as with a `Vec` (the table derefs to a slice); every
//! way to write (`IndexMut`, `get_mut`, `iter_mut`) checks the mark. A write below it
//! is a later unit changing a finished one, which the build architecture forbids
//! (docs/internals/design/build-architecture.md "The rule"). What happens then is set by
//! `VELT_UNITS_FREEZE`: `panic` stops with an ICE; anything else (the default, `report`) counts the
//! write by the caller's location, and [`report`] prints the counts at the end of a check, so one
//! run over the goldens lists every place that still writes across the boundary.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::{Deref, Index, IndexMut};
use std::panic::Location;

/// A `Vec<T>` indexed by `DefId` number, with a frozen prefix.
#[derive(Clone, Debug)]
pub(crate) struct DefTable<T> {
    items: Vec<T>,
    /// Entries below this index belong to finished units: 0 while nothing is frozen.
    frozen_below: u32,
}

impl<T> Default for DefTable<T> {
    fn default() -> Self {
        DefTable {
            items: Vec::new(),
            frozen_below: 0,
        }
    }
}

impl<T> DefTable<T> {
    /// Freeze every entry there is now.
    pub(crate) fn freeze(&mut self) {
        self.frozen_below = self.items.len() as u32;
    }

    /// Lift the freeze (the program-level stage after every unit, which may write anywhere).
    pub(crate) fn thaw(&mut self) {
        self.frozen_below = 0;
    }

    /// Is entry `i` in the frozen prefix?
    pub(crate) fn is_frozen(&self, i: usize) -> bool {
        i < self.frozen_below as usize
    }

    pub(crate) fn push(&mut self, item: T) {
        self.items.push(item);
    }

    #[track_caller]
    pub(crate) fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        self.check(i);
        self.items.get_mut(i)
    }

    /// Every entry, to write: a whole-table write while a prefix is frozen is counted once.
    #[track_caller]
    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        if self.frozen_below > 0 {
            violation(Location::caller());
        }
        self.items.iter_mut()
    }

    /// The entries, for `hir::Program`.
    pub(crate) fn into_vec(self) -> Vec<T> {
        self.items
    }

    #[track_caller]
    fn check(&self, i: usize) {
        if i < self.frozen_below as usize {
            violation(Location::caller());
        }
    }
}

impl<T> From<Vec<T>> for DefTable<T> {
    fn from(items: Vec<T>) -> Self {
        DefTable {
            items,
            frozen_below: 0,
        }
    }
}

impl<T> Deref for DefTable<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<'a, T> IntoIterator for &'a DefTable<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<T> Index<usize> for DefTable<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.items[i]
    }
}

impl<T> IndexMut<usize> for DefTable<T> {
    #[track_caller]
    fn index_mut(&mut self, i: usize) -> &mut T {
        self.check(i);
        &mut self.items[i]
    }
}

thread_local! {
    /// Writes below a freeze in this thread's checks, by the caller's location.
    static VIOLATIONS: RefCell<BTreeMap<String, usize>> = const { RefCell::new(BTreeMap::new()) };
}

/// A change to a finished unit that isn't a write to its tables (a slot it would need), counted as
/// a write there is.
#[track_caller]
pub(crate) fn crossing() {
    violation(Location::caller());
}

fn violation(at: &'static Location<'static>) {
    if std::env::var_os("VELT_UNITS_FREEZE").is_some_and(|v| v == "panic") {
        panic!("ICE: a later unit writes a frozen definition at {at}");
    }
    let key = format!("{}:{}", at.file(), at.line());
    VIOLATIONS.with(|v| *v.borrow_mut().entry(key).or_default() += 1);
}

/// The writes below a freeze counted so far in this thread, by location, and clears them.
pub(crate) fn take_violations() -> BTreeMap<String, usize> {
    VIOLATIONS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Prints the counted writes below a freeze to stderr (`velt-units:` lines), when there are any.
pub(crate) fn report() {
    let v = take_violations();
    if v.is_empty() {
        return;
    }
    eprintln!("velt-units: writes to frozen definitions");
    for (at, n) in v {
        eprintln!("velt-units: {n:>6}  {at}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_below_the_freeze_is_counted_where_it_is_made() {
        take_violations();
        let mut t: DefTable<u32> = vec![1, 2].into();
        t.freeze();
        t.push(3);
        t[2] = 30;
        assert!(take_violations().is_empty(), "a write above the freeze");
        t[0] = 10;
        let line = line!() - 1;
        let v = take_violations();
        assert_eq!(v.len(), 1, "{v:?}");
        let (at, n) = v.into_iter().next().unwrap();
        assert!(at.ends_with(&format!("def_table.rs:{line}")), "{at}");
        assert_eq!(n, 1);
        assert_eq!(t[0], 10, "reported, not refused");
    }

    #[test]
    fn a_whole_table_write_is_counted_while_frozen_and_thaw_lifts_the_freeze() {
        take_violations();
        let mut t: DefTable<u32> = vec![1, 2].into();
        t.freeze();
        let _ = t.iter_mut();
        assert_eq!(take_violations().len(), 1, "whole-table write while frozen");
        t.thaw();
        let _ = t.iter_mut();
        t[0] = 5;
        assert!(take_violations().is_empty());
    }
}
