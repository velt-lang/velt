//! Per-program-point ownership state: for every local, whether it is (maybe) wholly moved,
//! which sub-paths (fields / option payload) are moved, and whether it is (maybe)
//! uninitialized. `None` flow = unreachable.

use velt_common::Span;

/// Sub-path of a local: field indices; `UNWRAP` is the payload of a `T | null`, `VARIANT` the
/// member of a union.
pub(crate) type Path = Vec<u32>;
pub(crate) const UNWRAP: u32 = u32::MAX;
pub(crate) const VARIANT: u32 = u32::MAX - 1;

#[derive(Clone)]
pub(crate) struct State {
    pub moved: Vec<bool>,
    pub partial: Vec<Vec<Path>>,
    pub uninit: Vec<bool>,
    /// Every move that may have left each local moved here, and how (one per path that
    /// reaches this point: a soft move in a loop body and one before the loop, or one per
    /// branch). A use blames all of them, so each soft move used again becomes a share.
    pub moved_at: Vec<Vec<MoveSite>>,
    /// Where an escaping closure (maybe) captured each local by value (and the local holding
    /// that closure, while it is known), and whether such a closure assigns it: assigning the
    /// local afterwards, or reading it after a closure assigned it, needs a shared cell
    /// (`LocalDef::boxed`), unless the closure is gone (its holder's block ended).
    pub captured: Vec<Option<Captured>>,
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
            && self
                .moved_at
                .iter()
                .zip(&o.moved_at)
                .all(|(a, b)| same_sites(a, b))
    }
}

pub(crate) type Flow = Option<State>;

/// An escaping closure's by-value capture of a local: where, the local holding the closure
/// (while known), and whether the closure assigns the variable.
pub(crate) type Captured = (Span, Option<usize>, bool);

impl State {
    pub fn new(n: usize) -> Self {
        State {
            moved: vec![false; n],
            partial: vec![vec![]; n],
            uninit: vec![false; n],
            moved_at: vec![vec![]; n],
            captured: vec![None; n],
        }
    }

    /// Is any part of `path` of local `i` moved?
    pub fn overlaps(&self, i: usize, path: &[u32]) -> bool {
        self.moved[i]
            || self.partial[i]
                .iter()
                .any(|p| p.starts_with(path) || path.starts_with(p))
    }

    pub fn clear(&mut self, i: usize) {
        self.moved[i] = false;
        self.partial[i].clear();
        // Else a later join could blame (and soften) this stale move for a new one.
        self.moved_at[i].clear();
    }

    pub fn reinit(&mut self, i: usize, path: &[u32]) {
        if path.is_empty() {
            self.clear(i);
            self.uninit[i] = false;
            self.moved_at[i].clear();
        } else {
            self.partial[i].retain(|p| !p.starts_with(path));
        }
    }

    /// Local `holder` left its block: captures by the closure it held no longer count.
    pub fn drop_holder(&mut self, holder: usize) {
        for c in &mut self.captured {
            if c.is_some_and(|(_, h, _)| h == Some(holder)) {
                *c = None;
            }
        }
    }

    /// Local `holder` was moved: the closure it held lives on somewhere unknown.
    pub fn escape_holder(&mut self, holder: usize) {
        for (_, h, _) in self.captured.iter_mut().flatten() {
            if *h == Some(holder) {
                *h = None;
            }
        }
    }

    pub fn mark_moved(&mut self, i: usize, path: &[u32], at: Span, kind: MoveKind) {
        if path.is_empty() {
            self.moved[i] = true;
        } else if !self.partial[i].iter().any(|p| path.starts_with(p)) {
            self.partial[i].push(path.to_vec());
            self.partial[i].sort();
        }
        if !self.moved_at[i].contains(&(at, kind)) {
            self.moved_at[i].push((at, kind));
        }
    }
}

pub(crate) fn join(a: Flow, b: Flow) -> Flow {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut a), Some(b)) => {
            for i in 0..a.moved.len() {
                a.moved[i] |= b.moved[i];
                a.uninit[i] |= b.uninit[i];
                for p in &b.partial[i] {
                    if !a.partial[i].contains(p) {
                        a.partial[i].push(p.clone());
                    }
                }
                a.partial[i].sort();
                for site in &b.moved_at[i] {
                    if !a.moved_at[i].contains(site) {
                        a.moved_at[i].push(*site);
                    }
                }
                if let (Some(x), Some(y)) = (&mut a.captured[i], b.captured[i]) {
                    x.2 |= y.2;
                }
                if a.captured[i].is_none() {
                    a.captured[i] = b.captured[i];
                }
            }
            Some(a)
        }
    }
}
