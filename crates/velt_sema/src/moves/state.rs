//! Per-program-point ownership state: for every local, whether it is (maybe) wholly moved,
//! which sub-paths (fields / option payload) are moved, and whether it is (maybe)
//! uninitialized. `None` flow = unreachable.

use std::collections::{BTreeMap, BTreeSet};

use velt_common::Span;

/// Sub-path of a local: field indices; `UNWRAP` is the payload of a `T | null`, `VARIANT` the
/// member of a union.
pub(crate) type Path = Vec<u32>;
pub(crate) const UNWRAP: u32 = u32::MAX;
pub(crate) const VARIANT: u32 = u32::MAX - 1;

/// Only the locals something is known about are stored (a local absent from a set or map is
/// not moved, not uninitialized, not captured): a loop clones, joins and compares the state a
/// few times per pass, and a state holding every local of the function made a function of
/// many loops (and so many locals) quadratic.
#[derive(Clone, Default)]
pub(crate) struct State {
    /// The locals (maybe) wholly moved.
    moved: BTreeSet<usize>,
    /// Per local: its moved sub-paths, sorted; never an empty list.
    partial: BTreeMap<usize, Vec<Path>>,
    /// The locals (maybe) uninitialized.
    uninit: BTreeSet<usize>,
    /// Every move that may have left each local moved here, and how (one per path that
    /// reaches this point: a soft move in a loop body and one before the loop, or one per
    /// branch). A use blames all of them, so each soft move used again becomes a share. Never
    /// an empty list.
    moved_at: BTreeMap<usize, Vec<MoveSite>>,
    /// Where an escaping closure (maybe) captured each local by value (and the local holding
    /// that closure, while it is known), and whether such a closure assigns it: assigning the
    /// local afterwards, or reading it after a closure assigned it, needs a shared cell
    /// (`LocalDef::boxed`), unless the closure is gone (its holder's block ended).
    captured: BTreeMap<usize, Captured>,
}

/// How a place was moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveKind {
    Plain,
    /// Captured by value by an escaping closure.
    Closure,
    /// An argument of an async call that becomes a clone if the place is used again
    /// (`FnInfo::soft_moves`).
    Soft,
}

/// A move of a local: where, and how.
pub(crate) type MoveSite = (Span, MoveKind);

impl PartialEq for State {
    /// Move sites compare as sets (a loop's fixpoint must also have collected every site).
    fn eq(&self, o: &Self) -> bool {
        let same_sites = |a: &Vec<MoveSite>, b: &Vec<MoveSite>| {
            a.len() == b.len() && a.iter().all(|x| b.contains(x))
        };
        self.moved == o.moved
            && self.partial == o.partial
            && self.uninit == o.uninit
            && self.captured == o.captured
            && self.moved_at.len() == o.moved_at.len()
            && self
                .moved_at
                .iter()
                .zip(&o.moved_at)
                .all(|((i, a), (j, b))| i == j && same_sites(a, b))
    }
}

pub(crate) type Flow = Option<State>;

/// An escaping closure's by-value capture of a local: where, the local holding the closure
/// (while known), and whether the closure assigns the variable.
pub(crate) type Captured = (Span, Option<usize>, bool);

impl State {
    /// The state at a function's entry: nothing moved, uninitialized or captured. (`_locals`,
    /// the function's number of locals, is not needed by the sparse sets.)
    pub fn new(_locals: usize) -> Self {
        State::default()
    }

    /// How many facts the state holds (`crate::check_work`).
    pub fn entries(&self) -> usize {
        self.moved.len()
            + self.partial.len()
            + self.uninit.len()
            + self.moved_at.len()
            + self.captured.len()
    }

    /// Is any part of `path` of local `i` moved?
    pub fn overlaps(&self, i: usize, path: &[u32]) -> bool {
        self.moved.contains(&i)
            || self.partial.get(&i).is_some_and(|ps| {
                ps.iter()
                    .any(|p| p.starts_with(path) || path.starts_with(p))
            })
    }

    /// The moves that may have left local `i` moved here.
    pub fn moved_at(&self, i: usize) -> &[MoveSite] {
        self.moved_at.get(&i).map_or(&[], Vec::as_slice)
    }

    pub fn is_uninit(&self, i: usize) -> bool {
        self.uninit.contains(&i)
    }

    pub fn set_uninit(&mut self, i: usize, uninit: bool) {
        if uninit {
            self.uninit.insert(i);
        } else {
            self.uninit.remove(&i);
        }
    }

    /// The by-value capture of local `i` by an escaping closure, if any.
    pub fn captured(&self, i: usize) -> Option<Captured> {
        self.captured.get(&i).copied()
    }

    pub fn set_captured(&mut self, i: usize, c: Option<Captured>) {
        match c {
            Some(c) => self.captured.insert(i, c),
            None => self.captured.remove(&i),
        };
    }

    pub fn clear(&mut self, i: usize) {
        self.moved.remove(&i);
        self.partial.remove(&i);
        // Else a later join could blame (and soften) this stale move for a new one.
        self.moved_at.remove(&i);
    }

    pub fn reinit(&mut self, i: usize, path: &[u32]) {
        if path.is_empty() {
            self.clear(i);
            self.uninit.remove(&i);
        } else if let Some(ps) = self.partial.get_mut(&i) {
            ps.retain(|p| !p.starts_with(path));
            if ps.is_empty() {
                self.partial.remove(&i);
            }
        }
    }

    /// Local `holder` left its block: captures by the closure it held no longer count.
    pub fn drop_holder(&mut self, holder: usize) {
        self.captured.retain(|_, (_, h, _)| *h != Some(holder));
    }

    /// Local `holder` was moved: the closure it held lives on somewhere unknown.
    pub fn escape_holder(&mut self, holder: usize) {
        for (_, h, _) in self.captured.values_mut() {
            if *h == Some(holder) {
                *h = None;
            }
        }
    }

    pub fn mark_moved(&mut self, i: usize, path: &[u32], at: Span, kind: MoveKind) {
        if path.is_empty() {
            self.moved.insert(i);
        } else {
            let ps = self.partial.entry(i).or_default();
            if !ps.iter().any(|p| path.starts_with(p)) {
                ps.push(path.to_vec());
                ps.sort();
            }
        }
        let sites = self.moved_at.entry(i).or_default();
        if !sites.contains(&(at, kind)) {
            sites.push((at, kind));
        }
    }
}

pub(crate) fn join(a: Flow, b: Flow) -> Flow {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut a), Some(b)) => {
            // Only the locals `b` knows something about can change `a`.
            a.moved.extend(b.moved);
            a.uninit.extend(b.uninit);
            for (i, ps) in b.partial {
                let mine = a.partial.entry(i).or_default();
                for p in ps {
                    if !mine.contains(&p) {
                        mine.push(p);
                    }
                }
                mine.sort();
            }
            for (i, sites) in b.moved_at {
                let mine = a.moved_at.entry(i).or_default();
                for site in sites {
                    if !mine.contains(&site) {
                        mine.push(site);
                    }
                }
            }
            for (i, y) in b.captured {
                match a.captured.get_mut(&i) {
                    Some(x) => x.2 |= y.2,
                    None => {
                        a.captured.insert(i, y);
                    }
                }
            }
            Some(a)
        }
    }
}
