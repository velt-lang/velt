//! A C struct's aggregate is C's layout (#3094 "the layout guarantee"): velt/ffi's `Ptr.read` and
//! `Ptr.write` move its bytes to and from C memory (ptr_memory.rs). Fields in declaration
//! order at natural alignment, as `new_agg` lays out every aggregate, is C's rule on the 64-bit
//! targets; this checks it independently, from the C types of the fields, and that no field is
//! boxed or counted (a pointer to Velt's heap where C has the value). On wasm32, where C's
//! pointers, `size_t` and `ssize_t` are 4 bytes, sema refuses every operation that would expose
//! a struct holding one to C (`c_memory.rs`).

use velt_sema::hir::{self, FloatTy, IntTy, TyId, TyKind};

use super::{ice, Cx};
use crate::vir::{AggId, Ty};

impl Cx<'_> {
    /// Checks that `agg`, the aggregate of C struct `t`, is C's layout: an `ICE:` otherwise.
    pub(super) fn assert_c_layout(&self, t: TyId, agg: AggId) {
        let fields = self.c_struct_fields(t);
        let l = &self.aggs[agg.0 as usize];
        let (size, align, offsets) = self.c_layout(t);
        let at: Vec<u32> = l.fields.iter().map(|&(_, off)| off).collect();
        let held = fields
            .iter()
            .zip(&l.fields)
            .all(|(&f, &(vt, _))| self.c_held(f, vt));
        if (l.size, l.align) != (size, align) || at != offsets || !held {
            ice(format_args!(
                "C struct `{}` isn't C's layout: {:?}, where C has size {size}, align {align}, offsets {offsets:?}",
                self.type_name(t),
                l
            ));
        }
    }

    /// The field types of C struct `t`.
    fn c_struct_fields(&self, t: TyId) -> Vec<TyId> {
        match self.kind(t) {
            TyKind::Adt(d, _) => match self.hir.def(d) {
                hir::Def::Adt(a) if a.c_struct => a.fields.iter().map(|f| f.ty).collect(),
                _ => ice("C layout of a type that isn't a C struct"),
            },
            _ => ice("C layout of a type that isn't a C struct"),
        }
    }

    /// C's size, alignment and field offsets of C struct `t` on a 64-bit target.
    fn c_layout(&self, t: TyId) -> (u32, u32, Vec<u32>) {
        let (mut off, mut align, mut offsets) = (0u32, 1u32, vec![]);
        for f in self.c_struct_fields(t) {
            let (s, a) = self.c_size_align(f);
            off = off.next_multiple_of(a);
            offsets.push(off);
            off += s;
            align = align.max(a);
        }
        (off.next_multiple_of(align), align, offsets)
    }

    /// C's size and alignment of field type `t` on a 64-bit target.
    fn c_size_align(&self, t: TyId) -> (u32, u32) {
        let n = match self.kind(t) {
            TyKind::Bool | TyKind::Int(IntTy::I8 | IntTy::U8) => 1,
            TyKind::Int(IntTy::I16 | IntTy::U16) => 2,
            TyKind::Int(IntTy::I32 | IntTy::U32) | TyKind::Float(FloatTy::F32) => 4,
            TyKind::Int(_) | TyKind::Float(FloatTy::F64) | TyKind::Ptr(_) | TyKind::Option(..) => 8,
            TyKind::Adt(..) if self.c_struct(t) => {
                let (size, align, _) = self.c_layout(t);
                return (size, align);
            }
            k => ice(format_args!("C struct field of type {k:?}")),
        };
        (n, n)
    }

    /// Is a field of type `t` held as C holds it in VIR type `vt`: a scalar of its own width, a
    /// pointer as the word (`Ptr<T> | null` too, the null niche), a C struct as its aggregate?
    fn c_held(&self, t: TyId, vt: Ty) -> bool {
        match (self.kind(t), vt) {
            (TyKind::Ptr(_), Ty::Ptr) => true,
            (TyKind::Option(inner, _), Ty::Ptr) => matches!(self.kind(inner), TyKind::Ptr(_)),
            (TyKind::Adt(..), Ty::Agg(_)) => self.c_struct(t),
            (TyKind::Bool | TyKind::Int(_) | TyKind::Float(_), v) => v.is_scalar() && v != Ty::Ptr,
            _ => false,
        }
    }
}
