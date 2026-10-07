//! The alias analyses of `heap_sroa`'s value-semantics check (`flow`): which web locals hold
//! the same object as another one, forward over the copies between them.
//!
//! - **may**: some path makes them hold the same object (union at joins);
//! - **must**: every path does (intersection at joins). The must-aliases of a local form its
//!   equivalence class: a copy `a = b` puts `a` into `b`'s class, any other assignment takes
//!   `a` out of its class.

use super::flow::Event;

/// Alias rows: bit `j` of `rows[i]` says local `j` (may or must) hold `i`'s object. The rows are
/// symmetric, and never contain the local itself.
#[derive(Clone, PartialEq)]
pub(super) struct Aliases(pub Vec<u64>);

impl Aliases {
    fn forget(&mut self, i: usize) {
        let row = std::mem::take(&mut self.0[i]);
        for j in bits(row) {
            self.0[j] &= !(1 << i);
        }
    }

    /// The aliases after `ev`.
    pub fn step(&mut self, ev: Event) {
        match ev {
            Event::Kill(i) => self.forget(i),
            Event::Copy { dst, src } if dst != src => {
                self.forget(dst);
                let row = self.0[src];
                for j in bits(row) {
                    self.0[j] |= 1 << dst;
                }
                self.0[src] |= 1 << dst;
                self.0[dst] = row | (1 << src);
            }
            _ => {}
        }
    }

    fn union_with(&mut self, other: &Aliases) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            changed |= *b & !*a != 0;
            *a |= b;
        }
        changed
    }

    fn intersect_with(&mut self, other: &Aliases) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            changed |= *a & !*b != 0;
            *a &= b;
        }
        changed
    }
}

/// The indices of the set bits.
pub(super) fn bits(mut set: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (set != 0).then(|| {
            let i = set.trailing_zeros() as usize;
            set &= set - 1;
            i
        })
    })
}

/// May-alias rows at the start of every block.
pub(super) fn may(events: &[Vec<Event>], succs: &[Vec<usize>], n: usize) -> Vec<Aliases> {
    let mut alias_in = vec![Aliases(vec![0; n]); events.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (b, evs) in events.iter().enumerate() {
            let mut rows = alias_in[b].clone();
            for &ev in evs {
                rows.step(ev);
            }
            for &s in &succs[b] {
                changed |= alias_in[s].union_with(&rows);
            }
        }
    }
    alias_in
}

/// Must-alias rows at the start of every block (empty in unreachable blocks). The entry block
/// starts with no aliases; every other block starts unknown ("all") until a path reaches it.
pub(super) fn must(events: &[Vec<Event>], succs: &[Vec<usize>], n: usize) -> Vec<Aliases> {
    let mut alias_in: Vec<Option<Aliases>> = vec![None; events.len()];
    if let Some(entry) = alias_in.first_mut() {
        *entry = Some(Aliases(vec![0; n]));
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (b, evs) in events.iter().enumerate() {
            let Some(mut rows) = alias_in[b].clone() else {
                continue;
            };
            for &ev in evs {
                rows.step(ev);
            }
            for &s in &succs[b] {
                match &mut alias_in[s] {
                    Some(old) => changed |= old.intersect_with(&rows),
                    slot @ None => {
                        *slot = Some(rows.clone());
                        changed = true;
                    }
                }
            }
        }
    }
    alias_in
        .into_iter()
        .map(|rows| rows.unwrap_or_else(|| Aliases(vec![0; n])))
        .collect()
}
