//! The zero fill a `dead_fills` walk found, and which of its bytes have been written since
//! (module docs there).

use velt_vir::vir::{AggId, AggLayout, Ty};

use crate::fresh::access::{data_bytes, size, Access};

/// How the stores into the filled range see it.
#[derive(Clone, Copy, PartialEq)]
enum View {
    /// No store yet.
    None,
    /// Every store goes through the same object aggregate, at the start of the fill.
    Agg(AggId),
    /// Through other types or offsets: every byte holds data.
    Bytes,
}

/// The zero fill and the bytes of it written since.
pub(super) struct Fill {
    /// Its position (block, statement).
    pub at: (usize, usize),
    start: i128,
    written: Vec<bool>,
    view: View,
}

impl Fill {
    pub fn new(at: (usize, usize), start: i128, len: u32) -> Fill {
        Fill {
            at,
            start,
            written: vec![false; len as usize],
            view: View::None,
        }
    }

    /// The index of byte `off` in the fill.
    fn index(&self, off: i128) -> Option<usize> {
        let i = usize::try_from(off - self.start).ok()?;
        (i < self.written.len()).then_some(i)
    }

    /// Record a store; returns whether it wrote bytes of the fill.
    pub fn write(&mut self, aggs: &[AggLayout], a: &Access) -> bool {
        let end = a.lo + size(aggs, a.ty);
        let mut touched = false;
        for off in a.lo..end {
            if let Some(i) = self.index(off) {
                self.written[i] = true;
                touched = true;
            }
        }
        if !touched {
            return false;
        }
        let same = match a.view {
            Some(id) if a.base == self.start => Some(id),
            _ => None,
        };
        self.view = match (self.view, same) {
            (View::None, Some(id)) => View::Agg(id),
            (View::Agg(old), Some(id)) if old == id => View::Agg(id),
            _ => View::Bytes,
        };
        true
    }

    /// Whether every data byte `a` reads inside the fill has been written.
    pub fn readable(&self, aggs: &[AggLayout], a: &Access) -> bool {
        let ty = if a.through { Ty::Ptr } else { a.ty };
        let mut ok = true;
        data_bytes(aggs, ty, a.lo, &mut |off| {
            if let Some(i) = self.index(off) {
                ok &= self.written[i];
            }
        });
        ok
    }

    /// Whether every byte of the fill that holds data has been written.
    pub fn covered(&self, aggs: &[AggLayout]) -> bool {
        match self.view {
            View::None => false,
            View::Agg(id) if size(aggs, Ty::Agg(id)) == self.written.len() as i128 => {
                let mut ok = true;
                data_bytes(aggs, Ty::Agg(id), self.start, &mut |off| {
                    ok &= self.index(off).is_some_and(|i| self.written[i]);
                });
                ok
            }
            _ => self.written.iter().all(|&w| w),
        }
    }
}
