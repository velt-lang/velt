//! The bytes a place through a pointer into a new block reads or writes, and the bookkeeping of
//! which bytes of the fill have been written since (module docs of `dead_fills`).

use velt_vir::vir::{AggId, AggLayout, Place, Proj, Ty};

/// How a place relates to the tracked block.
pub(super) enum Touch {
    /// It does not go through a tracked pointer.
    Elsewhere,
    /// It mentions a tracked pointer in a way the scan does not follow (the pointer itself).
    Escape,
    /// It goes through a tracked pointer.
    At(Access),
}

/// A place through a pointer into the block.
pub(super) struct Access {
    /// Offset of the pointer the place goes through.
    base: i128,
    /// Offset of the bytes the place names.
    lo: i128,
    /// Their type.
    ty: Ty,
    /// The aggregate the place first dereferences as, if it never reinterprets (`Proj::Cast`).
    view: Option<AggId>,
    /// The place dereferences a pointer stored at `lo`: it reads those bytes and goes
    /// elsewhere.
    pub through: bool,
}

/// The access of place `p` whose local points at `base` into the block.
pub(super) fn of(aggs: &[AggLayout], base: i128, p: &Place) -> Touch {
    let Some(Proj::Deref(first)) = p.proj.first() else {
        return Touch::Escape;
    };
    let mut access = Access {
        base,
        lo: base,
        ty: *first,
        view: match first {
            Ty::Agg(id) => Some(*id),
            _ => None,
        },
        through: false,
    };
    for proj in &p.proj[1..] {
        match (proj, access.ty) {
            (Proj::Field(k), Ty::Agg(id)) => {
                let Some(&(ty, offset)) = aggs
                    .get(id.0 as usize)
                    .and_then(|a| a.fields.get(*k as usize))
                else {
                    return Touch::Escape;
                };
                access.lo += i128::from(offset);
                access.ty = ty;
            }
            (Proj::Cast(id), _) => {
                access.ty = Ty::Agg(*id);
                access.view = None;
            }
            (Proj::Deref(_), Ty::Ptr) => {
                access.through = true;
                break;
            }
            _ => return Touch::Escape,
        }
    }
    Touch::At(access)
}

fn size(aggs: &[AggLayout], ty: Ty) -> i128 {
    match ty {
        Ty::Agg(id) => aggs.get(id.0 as usize).map_or(0, |a| i128::from(a.size)),
        scalar => i128::from(scalar.scalar_size().unwrap_or(0)),
    }
}

/// Call `f` with every byte offset of a `ty` at `at` that holds data (not padding).
fn data_bytes(aggs: &[AggLayout], ty: Ty, at: i128, f: &mut impl FnMut(i128)) {
    match ty {
        Ty::Agg(id) => match aggs.get(id.0 as usize) {
            Some(layout) => {
                for &(field, offset) in &layout.fields {
                    data_bytes(aggs, field, at + i128::from(offset), f);
                }
            }
            None => (at..at + size(aggs, ty)).for_each(f),
        },
        scalar => (at..at + i128::from(scalar.scalar_size().unwrap_or(0))).for_each(f),
    }
}

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
