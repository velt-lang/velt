//! `VELT_UNITS_TOUCH` (#2943 M2 experiment): which frozen entries the app's stage reads, and
//! where it reads whole tables, to size an on-disk unit that is decoded on demand.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::Location;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static DEFS: RefCell<BTreeMap<&'static str, BTreeSet<usize>>> =
        const { RefCell::new(BTreeMap::new()) };
    static SCANS: RefCell<BTreeMap<String, usize>> = const { RefCell::new(BTreeMap::new()) };
    static TYPES: Cell<usize> = const { Cell::new(0) };
    static FROZEN: Cell<usize> = const { Cell::new(0) };
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

/// Start counting the app's stage of a check by units, against std's `frozen` definitions
/// and `types` types.
pub(crate) fn begin(frozen: usize, types: usize) {
    FROZEN.with(|f| f.set(frozen));
    start_types(types);
    start();
}

/// Report what `phase` of the app's stage read (when counting), and count the next one when
/// `more`.
pub(crate) fn phase(phase: &str, more: bool) {
    if !on() {
        return;
    }
    eprintln!("velt-touch: == {phase}");
    report(FROZEN.with(Cell::get));
    if more {
        let types = TYPES.with(Cell::get);
        start();
        start_types(types);
    }
}

/// The entries of `table` (its type name ends with `table`) read so far.
pub(crate) fn touched(table: &str) -> Vec<usize> {
    DEFS.with(|d| {
        (d.borrow().iter())
            .find(|(t, _)| t.ends_with(table))
            .map(|(_, s)| s.iter().copied().collect())
            .unwrap_or_default()
    })
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
    eprintln!(
        "velt-touch: {:>6} entries decoded on demand so far",
        super::decoded::get()
    );
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
