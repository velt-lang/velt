//! Type-based alias information (`!tbaa`) for arrays, so that LLVM can tell a store into an
//! array's elements from the array's header (`{ data, len, cap }`, `velt_vir::ARRAY_AGG_NAME`)
//! and keeps the data pointer and length in registers across element stores.
//!
//! Without it, LLVM must reload them after every element store unless the array is a `noalias`
//! param, which a counted array (one the program shares between variables) never is: a byte
//! loop in a server then loads the header once per byte, and a vector loop once per vector.
//!
//! Three tags, under one root, and every other access untagged (untagged accesses may alias
//! anything, so they stay correct whatever they touch):
//! - **header**: a field of an array header, wherever the header lives (behind an array
//!   value, inline in an object or async frame, in a local);
//! - **element**: a scalar element read or written through a pointer computed from a header's
//!   data field ([`element_pointers`]), the vector loads and stores of the SIMD helpers, and the
//!   byte reads of a string's text (through `velt_vir::STR_TEXT_AGG_NAME`, #1669). One tag for
//!   every element type: a typed array reads the bytes of a `u8[]` as other types.
//! - **object field**: a scalar field of a class object (`velt_vir::OBJECT_AGG_SUFFIX`). One tag
//!   for every class: an object is read through its own layout and its base classes' (prefixes
//!   of it). A typed array's element access reads its buffer from such a field, so this keeps
//!   the field in a register across the element stores too.
//!
//! No two of them touch the same memory: element memory is an array's data buffer of scalars or
//! a string's text, and no header lives in a buffer of scalars (an array of arrays holds pointers to headers, an
//! array of aggregates is read through aggregate places, which get no element tag). A class
//! object is a heap object (or a local `heap_sroa` made of one), never inside a data buffer; a
//! header stored inline in an object is reached by projecting its own fields, which get the
//! header tag, and an object layout has no overlapping fields (async frames and enum views,
//! which do, are not object layouts and stay untagged). The reference count before an object
//! is read through an offset pointer and stays untagged. A string's text is its own buffer, or
//! the bytes of the 24-byte string value itself (an inline string), wherever that value lives:
//! a string field of an object is reached by projecting the string's own words, which get no
//! tag (as do whole-string copies, `memcpy`), so a store that changes an inline string's text
//! stays untagged and may alias the text reads.

use velt_vir::vir::{self, AggId, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty};

/// Which tag a memory access carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Header,
    Element,
    Field,
}

impl Access {
    /// The `, !tbaa !{...}` suffix of a load or store. The nodes are written inline: identical
    /// tuples are one node to LLVM, so no numbering is shared with the debug metadata.
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            Access::Header => {
                ", !tbaa !{!{!\"velt array header\", !{!\"velt\"}, i64 0}, \
                 !{!\"velt array header\", !{!\"velt\"}, i64 0}, i64 0}"
            }
            Access::Element => {
                ", !tbaa !{!{!\"velt array element\", !{!\"velt\"}, i64 0}, \
                 !{!\"velt array element\", !{!\"velt\"}, i64 0}, i64 0}"
            }
            Access::Field => {
                ", !tbaa !{!{!\"velt object field\", !{!\"velt\"}, i64 0}, \
                 !{!\"velt object field\", !{!\"velt\"}, i64 0}, i64 0}"
            }
        }
    }
}

/// Per aggregate of the program: whether it is a class object's.
pub(crate) fn object_aggs(program: &vir::Program) -> Vec<bool> {
    program
        .aggs
        .iter()
        .map(|a| a.name.ends_with(velt_vir::OBJECT_AGG_SUFFIX))
        .collect()
}

/// The aggregate string bytes are read through, if the program reads any.
pub(crate) fn text_agg(program: &vir::Program) -> Option<AggId> {
    program
        .aggs
        .iter()
        .position(|a| a.name == velt_vir::STR_TEXT_AGG_NAME)
        .map(|i| AggId(i as u32))
}

/// The program's array header aggregate, if it has arrays.
pub(crate) fn array_agg(program: &vir::Program) -> Option<AggId> {
    program
        .aggs
        .iter()
        .position(|a| a.name == velt_vir::ARRAY_AGG_NAME)
        .map(|i| AggId(i as u32))
}

/// Per local of `function`: whether it only ever holds a pointer into an array's data buffer.
/// That is a `Ptr` local that is not a param, whose address is never taken, and that is only
/// assigned `ptradd` of a header's data field (`elem_place` in velt_vir), or `ptradd` or a copy
/// of another such local. Computed as the largest such set: start from every candidate and
/// drop the ones with an assignment of another kind until nothing changes.
pub(crate) fn element_pointers(
    program: &vir::Program,
    function: &vir::Function,
    array: Option<AggId>,
) -> Vec<bool> {
    let n = function.locals.len();
    let mut elem: Vec<bool> = (0..n)
        .map(|i| i >= function.params.len() && function.locals[i].ty == Ty::Ptr)
        .collect();
    let Some(array) = array else {
        return vec![false; n];
    };
    // Assignments to whole locals; anything else that writes a local or exposes it rules it out.
    let mut defs: Vec<(usize, &Rvalue)> = vec![];
    for block in &function.blocks {
        for stmt in &block.stmts {
            if let Stmt::Assign(place, rvalue) = stmt {
                if place.proj.is_empty() {
                    defs.push((place.local.0 as usize, rvalue));
                }
                if let Rvalue::AddrOf(p) = rvalue {
                    if p.proj.is_empty() {
                        elem[p.local.0 as usize] = false;
                    }
                }
            }
        }
        if let Terminator::Call {
            dest: Some(place), ..
        } = &block.term
        {
            if place.proj.is_empty() {
                elem[place.local.0 as usize] = false;
            }
        }
    }
    // A candidate never assigned holds no pointer this function made.
    let mut assigned = vec![false; n];
    for &(local, _) in &defs {
        assigned[local] = true;
    }
    for (e, a) in elem.iter_mut().zip(&assigned) {
        *e &= *a;
    }
    loop {
        let mut changed = false;
        for &(local, rvalue) in &defs {
            if elem[local] && !from_element_pointer(program, function, array, &elem, rvalue) {
                elem[local] = false;
                changed = true;
            }
        }
        if !changed {
            return elem;
        }
    }
}

/// Whether `rvalue` is a pointer into an array's data buffer, given the locals known so far.
fn from_element_pointer(
    program: &vir::Program,
    function: &vir::Function,
    array: AggId,
    elem: &[bool],
    rvalue: &Rvalue,
) -> bool {
    let base = match rvalue {
        Rvalue::Binary(vir::BinOp::PtrAdd, Operand::Copy(base), _) => base,
        Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => return elem[p.local.0 as usize],
        _ => return false,
    };
    if base.proj.is_empty() {
        return elem[base.local.0 as usize];
    }
    is_data_field(program, function, array, base)
}

/// Whether `place` is field 0 (`data`) of an array header.
fn is_data_field(
    program: &vir::Program,
    function: &vir::Function,
    array: AggId,
    place: &Place,
) -> bool {
    let Some((Proj::Field(0), before)) = place.proj.split_last() else {
        return false;
    };
    container(program, function, place.local, before) == Some(Ty::Agg(array))
}

/// The type of the place `local` + `proj`, if the projections are well-formed.
pub(crate) fn container(
    program: &vir::Program,
    function: &vir::Function,
    local: vir::Local,
    proj: &[Proj],
) -> Option<Ty> {
    let mut ty = function.locals.get(local.0 as usize)?.ty;
    for p in proj {
        ty = match (p, ty) {
            (Proj::Deref(pointee), Ty::Ptr) => *pointee,
            (Proj::Cast(id), _) => Ty::Agg(*id),
            (Proj::Field(n), Ty::Agg(id)) => {
                program.aggs.get(id.0 as usize)?.fields.get(*n as usize)?.0
            }
            _ => return None,
        };
    }
    Some(ty)
}
