//! The bytes a place through a pointer into the new block reads or writes (module docs of
//! `fresh`).

use velt_vir::vir::{AggId, AggLayout, Place, Proj, Ty};

/// How a place relates to the tracked block.
pub(crate) enum Touch {
    /// It does not go through a tracked pointer.
    Elsewhere,
    /// It mentions a tracked pointer in a way the walk does not follow (the pointer itself).
    Escape,
    /// It goes through a tracked pointer.
    At(Access),
}

/// A place through a pointer into the block.
pub(crate) struct Access {
    /// Offset of the pointer the place goes through.
    pub base: i128,
    /// Offset of the bytes the place names.
    pub lo: i128,
    /// Their type.
    pub ty: Ty,
    /// The aggregate the place first dereferences as, if it never reinterprets (`Proj::Cast`).
    pub view: Option<AggId>,
    /// The place dereferences a pointer stored at `lo`: it reads those bytes and goes
    /// elsewhere.
    pub through: bool,
}

/// The access of place `p` whose local points at `base` into the block.
pub(crate) fn of(aggs: &[AggLayout], base: i128, p: &Place) -> Touch {
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

/// The size of a `ty` in bytes.
pub(crate) fn size(aggs: &[AggLayout], ty: Ty) -> i128 {
    match ty {
        Ty::Agg(id) => aggs.get(id.0 as usize).map_or(0, |a| i128::from(a.size)),
        scalar => i128::from(scalar.scalar_size().unwrap_or(0)),
    }
}

/// Call `f` with every byte offset of a `ty` at `at` that holds data (not padding).
pub(crate) fn data_bytes(aggs: &[AggLayout], ty: Ty, at: i128, f: &mut impl FnMut(i128)) {
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
