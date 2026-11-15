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

impl<T> DefTable<T> {
    /// Entry `i`, if there is one.
    #[track_caller]
    pub(crate) fn get(&self, i: usize) -> Option<&T> {
        self.touched(i);
        self.items.get(i)
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// Every entry, to read.
    #[track_caller]
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, T> {
        self.scanned();
        self.items.iter()
    }

    #[track_caller]
    fn touched(&self, i: usize) {
        if i < self.frozen_below as usize && touch::on() {
            touch::def(std::any::type_name::<T>(), i, Location::caller());
        }
    }

    #[track_caller]
    fn scanned(&self) {
        if self.frozen_below > 0 && touch::on() {
            touch::scan(std::any::type_name::<T>(), Location::caller());
        }
    }
}

impl<T> Deref for DefTable<T> {
    type Target = [T];
    #[track_caller]
    fn deref(&self) -> &[T] {
        self.scanned();
        &self.items
    }
}

impl<'a, T> IntoIterator for &'a DefTable<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    #[track_caller]
    fn into_iter(self) -> Self::IntoIter {
        self.scanned();
        self.items.iter()
    }
}

impl<T> Index<usize> for DefTable<T> {
    type Output = T;
    #[track_caller]
    fn index(&self, i: usize) -> &T {
        self.touched(i);
        &self.items[i]
    }
}

/// `VELT_UNITS_TOUCH` (#2943 M2 experiment): which frozen entries the app's stage reads, and
/// where it reads whole tables, to size an on-disk unit that is decoded on demand.
pub(crate) mod touch {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::panic::Location;

    thread_local! {
        static ON: Cell<bool> = const { Cell::new(false) };
        static DEFS: RefCell<BTreeMap<&'static str, BTreeSet<usize>>> =
            const { RefCell::new(BTreeMap::new()) };
        static SCANS: RefCell<BTreeMap<String, usize>> = const { RefCell::new(BTreeMap::new()) };
        static TYPES: Cell<usize> = const { Cell::new(0) };
        static TYS: RefCell<BTreeSet<usize>> = const { RefCell::new(BTreeSet::new()) };
        static BY_SITE: RefCell<BTreeMap<String, BTreeSet<usize>>> =
            const { RefCell::new(BTreeMap::new()) };
    }

    /// A read of type `t`: counted when it is one of std's (below `start_types`' count).
    pub(crate) fn ty(t: usize) {
        if t < TYPES.with(Cell::get) {
            TYS.with(|s| s.borrow_mut().insert(t));
        }
    }

    /// Count reads of the first `n` types (std's unit's).
    pub(crate) fn start_types(n: usize) {
        TYPES.with(|c| c.set(n));
        TYS.with(|s| s.borrow_mut().clear());
    }

    pub(crate) fn on() -> bool {
        ON.with(Cell::get)
    }

    /// Start counting (the app's stage), when `VELT_UNITS_TOUCH` is set.
    pub(crate) fn start() {
        ON.with(|o| o.set(std::env::var_os("VELT_UNITS_TOUCH").is_some()));
        DEFS.with(|d| d.borrow_mut().clear());
        SCANS.with(|s| s.borrow_mut().clear());
        BY_SITE.with(|s| s.borrow_mut().clear());
    }

    pub(crate) fn def(table: &'static str, i: usize, at: &'static Location<'static>) {
        DEFS.with(|d| d.borrow_mut().entry(table).or_default().insert(i));
        let key = format!("{} {}:{}", short(table), at.file(), at.line());
        BY_SITE.with(|b| b.borrow_mut().entry(key).or_default().insert(i));
    }

    pub(crate) fn scan(table: &'static str, at: &'static Location<'static>) {
        let key = format!("{} {}:{}", short(table), at.file(), at.line());
        SCANS.with(|s| *s.borrow_mut().entry(key).or_default() += 1);
    }

    fn short(t: &str) -> &str {
        t.rsplit("::").next().unwrap_or(t)
    }

    /// Print what was counted (`velt-touch:` lines) against `frozen` entries, and stop.
    pub(crate) fn report(frozen: usize) {
        if !on() {
            return;
        }
        ON.with(|o| o.set(false));
        let types = TYPES.with(Cell::get);
        let read = TYS.with(|s| s.borrow().len());
        eprintln!("velt-touch: {read:>6} of {types} std types read");
        DEFS.with(|d| {
            for (t, set) in d.borrow().iter() {
                eprintln!(
                    "velt-touch: {:>6} of {frozen} frozen entries read  {}",
                    set.len(),
                    short(t)
                );
            }
        });
        BY_SITE.with(|s| {
            // The working set: what the sites that read under half the entries read together.
            for cut in [250, 500, 1000, 2000, frozen / 2] {
                let mut set = BTreeSet::new();
                let mut sites = 0;
                for d in s.borrow().values().filter(|d| d.len() < cut) {
                    set.extend(d.iter().copied());
                    sites += 1;
                }
                eprintln!(
                    "velt-touch: {:>6} entries read by the {sites} sites that read under {cut}",
                    set.len()
                );
            }
            let mut v: Vec<_> = s
                .borrow()
                .iter()
                .map(|(k, d)| (d.len(), k.clone()))
                .collect();
            v.sort_by(|a, b| b.cmp(a));
            for (n, k) in v.into_iter().take(40) {
                eprintln!("velt-touch: site {n:>6}  {k}");
            }
        });
        SCANS.with(|s| {
            let mut v: Vec<_> = s.borrow().iter().map(|(k, n)| (*n, k.clone())).collect();
            v.sort_by(|a, b| b.cmp(a));
            for (n, k) in v {
                eprintln!("velt-touch: scan {n:>6}  {k}");
            }
        });
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
