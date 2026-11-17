//! A table indexed by `DefId` whose first entries can be frozen (#2943 M1, units in sema).
//!
//! Units are checked in dependency order, so a finished unit's definitions are a prefix of the
//! table: `frozen_below` marks it. Reads are as with a `Vec` (indexing, `get`, `iter`); every
//! way to write (`IndexMut`, `get_mut`, `iter_mut`) checks the mark. A write below it
//! is a later unit changing a finished one, which the build architecture forbids
//! (docs/internals/design/build-architecture.md "The rule"). What happens then is set by
//! `VELT_UNITS_FREEZE`: `panic` stops with an ICE; anything else (the default, `report`) counts the
//! write by the caller's location, and [`report`] prints the counts at the end of a check, so one
//! run over the goldens lists every place that still writes across the boundary.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::{Index, IndexMut};
use std::panic::Location;
use std::rc::Rc;

use super::slots::Slots;

/// A `Vec<T>` indexed by `DefId` number, with a frozen prefix, whose entries may be decoded on
/// first use from a unit kept on disk ([`Lazy`]).
#[derive(Clone)]
pub(crate) struct DefTable<T> {
    items: Slots<T>,
    /// Where the entries not decoded yet are (`None`: every entry is there).
    lazy: Option<Rc<Lazy<T>>>,
    /// Entries below this index belong to finished units: 0 while nothing is frozen.
    frozen_below: u32,
    /// The same, kept through `thaw` (`touch`: what the app's stage reads of finished units).
    unit_below: u32,
}

/// EXPERIMENT (#2943 M2): the encoded entries of a table read from a unit, each decoded on its
/// first use (`units::codec`, `DefTable`'s codec).
pub(crate) struct Lazy<T> {
    src: Rc<crate::units::codec::Src>,
    /// Where the first entry starts in `src`'s bytes, and where each ends.
    start: usize,
    ends: Vec<usize>,
    decode: fn(&crate::units::codec::Src, std::ops::Range<usize>) -> T,
}

impl<T> Lazy<T> {
    pub(crate) fn new(
        src: Rc<crate::units::codec::Src>,
        start: usize,
        ends: Vec<usize>,
        decode: fn(&crate::units::codec::Src, std::ops::Range<usize>) -> T,
    ) -> Self {
        Lazy {
            src,
            start,
            ends,
            decode,
        }
    }

    /// Entry `i`, decoded.
    fn get(&self, i: usize) -> T {
        let lo = match i {
            0 => self.start,
            _ => self.ends[i - 1],
        };
        (self.decode)(&self.src, lo..self.ends[i])
    }
}

impl<T> std::fmt::Debug for Lazy<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lazy")
    }
}

impl<T> std::fmt::Debug for DefTable<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DefTable({} entries)", self.len())
    }
}

impl<T> Default for DefTable<T> {
    fn default() -> Self {
        DefTable {
            items: Slots::default(),
            lazy: None,
            frozen_below: 0,
            unit_below: 0,
        }
    }
}

impl<T> DefTable<T> {
    /// A table of `n` entries, each decoded by `lazy` when first used.
    pub(crate) fn lazy(n: usize, lazy: Lazy<T>) -> Self {
        DefTable {
            items: Slots::empty(n),
            lazy: Some(Rc::new(lazy)),
            frozen_below: 0,
            unit_below: 0,
        }
    }

    /// Entry `i`, decoded if it wasn't.
    #[inline]
    fn at(&self, i: usize) -> &T {
        match self.items.get(i) {
            Some(x) => x,
            None => self.decode(i),
        }
    }

    #[cold]
    fn decode(&self, i: usize) -> &T {
        let lazy = self
            .lazy
            .as_ref()
            .expect("ICE: a definition table entry left empty");
        self.items.set(i, lazy.get(i));
        decoded::count();
        self.items.get(i).expect("ICE: decoded entry")
    }

    /// Entry `i` to write, decoded if it wasn't.
    fn at_mut(&mut self, i: usize) -> &mut T {
        self.at(i);
        self.items.get_mut(i).expect("ICE: decoded entry")
    }

    /// Decode every entry not decoded yet.
    fn decode_all(&self) {
        for i in 0..self.items.len() {
            self.at(i);
        }
    }

    /// Freeze every entry there is now.
    pub(crate) fn freeze(&mut self) {
        self.frozen_below = self.items.len() as u32;
        self.unit_below = self.frozen_below;
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
        self.touched(i);
        self.check(i);
        (i < self.items.len()).then(|| self.at_mut(i))
    }

    /// Every entry, to write: a whole-table write while a prefix is frozen is counted once.
    #[track_caller]
    pub(crate) fn iter_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = &mut T> + ExactSizeIterator {
        self.scanned();
        if self.frozen_below > 0 {
            violation(Location::caller());
        }
        self.decode_all();
        let n = self.items.len();
        let items: *mut Slots<T> = &mut self.items;
        // SAFETY: each index is yielded once, so the `&mut`s don't overlap.
        (0..n).map(move |i| unsafe { (*items).get_mut(i).expect("ICE: decoded entry") })
    }

    /// The entries `keep` keeps, to write (the program's, `Ctx::program_filter`).
    #[track_caller]
    pub(crate) fn iter_mut_where<'s>(
        &'s mut self,
        keep: &dyn Fn(usize) -> bool,
    ) -> impl Iterator<Item = &'s mut T> {
        if self.frozen_below > 0 {
            violation(Location::caller());
        }
        let n = self.items.len();
        let kept: Vec<usize> = (0..n).filter(|&i| keep(i)).collect();
        for &i in &kept {
            self.at(i);
        }
        let items: *mut Slots<T> = &mut self.items;
        // SAFETY: each index is yielded once, so the `&mut`s don't overlap.
        kept.into_iter()
            .map(move |i| unsafe { (*items).get_mut(i).expect("ICE: decoded entry") })
    }

    /// Entry `i`, uncounted (the probe's own reads).
    pub(crate) fn items_at(&self, i: usize) -> &T {
        self.at(i)
    }

    /// Every entry, uncounted.
    pub(crate) fn items_all(&self) -> Iter<'_, T> {
        Iter {
            table: self,
            front: 0,
            back: self.items.len(),
        }
    }

    /// The entries, those not decoded yet as `fill` makes them (not decoded).
    pub(crate) fn into_vec_filling(self, fill: impl Fn(usize) -> T) -> Vec<T> {
        self.items.into_vec_filling(fill)
    }

    /// The entries, for `hir::Program`.
    pub(crate) fn into_vec(self) -> Vec<T> {
        self.decode_all();
        self.items.into_vec()
    }

    #[track_caller]
    fn check(&self, i: usize) {
        if i < self.frozen_below as usize {
            violation(Location::caller());
        }
    }
}

/// How many entries were decoded on demand (`VELT_UNITS_TOUCH`'s report).
pub(crate) mod decoded {
    use std::cell::Cell;
    thread_local! {
        static N: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn count() {
        N.with(|n| n.set(n.get() + 1));
    }
    pub(crate) fn get() -> usize {
        N.with(Cell::get)
    }
}

impl<T> From<Vec<T>> for DefTable<T> {
    fn from(items: Vec<T>) -> Self {
        DefTable {
            items: items.into_iter().collect(),
            lazy: None,
            frozen_below: 0,
            unit_below: 0,
        }
    }
}

impl<T> DefTable<T> {
    /// Entry `i`, if there is one.
    #[track_caller]
    pub(crate) fn get(&self, i: usize) -> Option<&T> {
        self.touched(i);
        (i < self.items.len()).then(|| self.at(i))
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// Every entry, to read (each decoded as the iteration reaches it: one skipped, by `nth`
    /// or `skip`, is not).
    #[track_caller]
    pub(crate) fn iter(&self) -> Iter<'_, T> {
        self.scanned();
        self.items_all()
    }

    #[track_caller]
    fn touched(&self, i: usize) {
        if i < self.unit_below as usize && touch::on() {
            touch::def(std::any::type_name::<T>(), i, Location::caller());
        }
    }

    #[track_caller]
    fn scanned(&self) {
        if self.unit_below > 0 && touch::on() {
            touch::scan(std::any::type_name::<T>(), Location::caller());
        }
    }
}

/// [`DefTable::iter`]'s iterator.
pub(crate) struct Iter<'a, T> {
    table: &'a DefTable<T>,
    front: usize,
    back: usize,
}

impl<T> Clone for Iter<'_, T> {
    fn clone(&self) -> Self {
        Iter { ..*self }
    }
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;
    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        if self.front >= self.back {
            return None;
        }
        self.front += 1;
        Some(self.table.at(self.front - 1))
    }
    fn nth(&mut self, n: usize) -> Option<&'a T> {
        self.front = self.front.saturating_add(n).min(self.back);
        self.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.back - self.front;
        (n, Some(n))
    }
}

impl<T> DoubleEndedIterator for Iter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front >= self.back {
            return None;
        }
        self.back -= 1;
        Some(self.table.at(self.back))
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}

impl<'a, T> IntoIterator for &'a DefTable<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;
    #[track_caller]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> Index<usize> for DefTable<T> {
    type Output = T;
    #[track_caller]
    fn index(&self, i: usize) -> &T {
        self.touched(i);
        self.at(i)
    }
}

#[path = "def_table_touch.rs"]
pub(crate) mod touch;

impl<T> IndexMut<usize> for DefTable<T> {
    #[track_caller]
    fn index_mut(&mut self, i: usize) -> &mut T {
        self.touched(i);
        self.check(i);
        self.at_mut(i)
    }
}

thread_local! {
    /// Writes below a freeze in this thread's checks, by the caller's file and line (made text
    /// only when [`take_violations`] reports them: a check over a frozen unit counts thousands).
    static VIOLATIONS: RefCell<BTreeMap<(&'static str, u32), usize>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// `VELT_UNITS_FREEZE=panic`: a write below a freeze panics, read once.
fn freeze_panics() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VELT_UNITS_FREEZE").is_some_and(|v| v == "panic"))
}

/// A change to a finished unit that isn't a write to its tables (a slot it would need), counted as
/// a write there is.
#[track_caller]
pub(crate) fn crossing() {
    violation(Location::caller());
}

fn violation(at: &'static Location<'static>) {
    if freeze_panics() {
        panic!("ICE: a later unit writes a frozen definition at {at}");
    }
    let key = (at.file(), at.line());
    VIOLATIONS.with(|v| *v.borrow_mut().entry(key).or_default() += 1);
}

/// The writes below a freeze counted so far in this thread, by location (`file:line`), and
/// clears them.
pub(crate) fn take_violations() -> BTreeMap<String, usize> {
    let v = VIOLATIONS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    v.into_iter()
        .map(|((file, line), n)| (format!("{file}:{line}"), n))
        .collect()
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
