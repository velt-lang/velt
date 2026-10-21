//! Interface adapters (hir_encodings.md "Interface adapters"): a value of `I<A>` converted to
//! `I<B>` is a counted `<I> adapter` struct wrapping the original interface value. In Node the
//! two are one object, so what tells objects apart looks through the adapter at the value it
//! wraps: `===` (same.rs), `instanceof` and the downcast after it (class_test.rs).
//!
//! An adapter's table says `ASK_INNER` in its class id word (glue/vtable.rs), and its data
//! pointer points at the struct, whose one field is the wrapped interface value. A program
//! without adapters compiles as before.

use velt_sema::hir::{AdtKind, TyId, TyKind};

use super::operand::proj;
use super::{cint, Cx, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// The class id word of an adapter's table: the value it wraps says what the object is.
pub(super) const ASK_INNER: u64 = u64::MAX - 1;

impl Cx<'_> {
    /// Is `ty` (an implementation's self type) an interface adapter: the hidden struct
    /// `<I> adapter` sema makes, whose one field is an interface value? A source type's name
    /// has no space, so the name is the adapter's alone.
    pub(super) fn is_adapter(&self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.kind(ty) else {
            return false;
        };
        let a = self.adt_def(d);
        a.kind == AdtKind::Struct
            && a.name.ends_with(" adapter")
            && a.fields.len() == 1
            && matches!(self.kind(a.fields[0].ty), TyKind::Dyn(..))
    }

    /// Does the program convert an interface value to another instance of its interface?
    fn has_adapters(&mut self) -> bool {
        if let Some(b) = self.lay.adapters {
            return b;
        }
        let b = self.hir.impls.iter().any(|i| self.is_adapter(i.ty));
        self.lay.adapters = Some(b);
        b
    }

    /// Values of the interface types adapters wrap are compared by identity too: each keeps
    /// the one object it was made from (`make_dyn`), so two adapters of it wrap the same one.
    fn note_wrapped_identity(&mut self) {
        let wrapped: Vec<TyId> = self
            .hir
            .impls
            .iter()
            .filter(|i| self.is_adapter(i.ty))
            .filter_map(|i| match self.kind(i.ty) {
                TyKind::Adt(d, _) => Some(self.adt_def(d).fields[0].ty),
                _ => None,
            })
            .collect();
        for t in wrapped {
            self.note_identity(t);
        }
    }
}

impl FnLower<'_, '_> {
    /// The interface value of type `ty` at `place`, or the value its adapters wrap (a place of
    /// the same layout: every interface value is a data and a table pointer).
    pub(super) fn through_adapters(&mut self, place: &Place, ty: TyId) -> Place {
        if !self.cx.has_adapters() {
            return place.clone();
        }
        self.cx.note_wrapped_identity();
        let vt = self.cx.ty(ty);
        let cur = Place::local(self.temp(vt));
        self.assign(cur.clone(), Rvalue::Use(Operand::Copy(place.clone())));
        let (head, body, done) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head);
        self.switch_to(head);
        let id = self.vtable_class_id(Operand::Copy(proj(&cur, Proj::Field(1))));
        let ask = cint(ASK_INNER as i128, Ty::U64);
        let wraps = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, ask));
        self.branch(wraps, body, done);
        self.switch_to(body);
        let data = self.operand_place(Operand::Copy(proj(&cur, Proj::Field(0))), Ty::Ptr);
        let inner = proj(&data, Proj::Deref(vt));
        self.assign(cur.clone(), Rvalue::Use(Operand::Copy(inner)));
        self.goto(head);
        self.switch_to(done);
        cur
    }

    /// The object of the owned interface value at `p` (of type `ty`, the downcast of a narrowed
    /// value): its data pointer, or, when adapters wrap it, a new reference to the object they
    /// wrap, the adapters released.
    pub(super) fn owned_through_adapters(&mut self, p: Place, ty: TyId) -> Operand {
        let data = Operand::Copy(proj(&p, Proj::Field(0)));
        if !self.cx.has_adapters() {
            return data;
        }
        let inner = self.through_adapters(&p, ty);
        let out = self.temp(Ty::Ptr);
        self.assign(Place::local(out), Rvalue::Use(data.clone()));
        let found = Operand::Copy(proj(&inner, Proj::Field(0)));
        let wrapped = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, found, data));
        let (yes, done) = (self.new_block(), self.new_block());
        self.branch(wrapped, yes, done);
        self.switch_to(yes);
        let shared = self.share_value(Operand::Copy(inner), ty);
        let shared = self.place_of(shared, ty);
        self.assign(
            Place::local(out),
            Rvalue::Use(Operand::Copy(proj(&shared, Proj::Field(0)))),
        );
        self.drop_glue(p, ty);
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(out))
    }
}
