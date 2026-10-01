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
    /// Where each maybe-moved local was (last) moved, and how.
    pub moved_at: Vec<Option<(Span, MoveKind)>>,
    /// Where an escaping closure (maybe) captured each local by value (and the local holding
    /// that closure, while it is known): assigning the local afterwards would not reach the
    /// closure's copy, unless the closure is gone (its holder's block ended).
    pub captured: Vec<Option<(Span, Option<usize>)>>,
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

impl PartialEq for State {
    fn eq(&self, o: &Self) -> bool {
        self.moved == o.moved
            && self.partial == o.partial
            && self.uninit == o.uninit
            && self.captured == o.captured
    }
}

pub(crate) type Flow = Option<State>;

impl State {
    pub fn new(n: usize) -> Self {
        State {
            moved: vec![false; n],
            partial: vec![vec![]; n],
            uninit: vec![false; n],
            moved_at: vec![None; n],
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
        self.moved_at[i] = None;
    }

    pub fn reinit(&mut self, i: usize, path: &[u32]) {
        if path.is_empty() {
            self.clear(i);
            self.uninit[i] = false;
            self.moved_at[i] = None;
        } else {
            self.partial[i].retain(|p| !p.starts_with(path));
        }
    }

    /// Local `holder` left its block: captures by the closure it held no longer count.
    pub fn drop_holder(&mut self, holder: usize) {
        for c in &mut self.captured {
            if c.is_some_and(|(_, h)| h == Some(holder)) {
                *c = None;
            }
        }
    }

    /// Local `holder` was moved: the closure it held lives on somewhere unknown.
    pub fn escape_holder(&mut self, holder: usize) {
        for (_, h) in self.captured.iter_mut().flatten() {
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
        self.moved_at[i] = Some((at, kind));
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
                if a.moved_at[i].is_none() {
                    a.moved_at[i] = b.moved_at[i];
                }
                if a.captured[i].is_none() {
                    a.captured[i] = b.captured[i];
                }
            }
            Some(a)
        }
    }
}
