//! Runtime externs and boxed types (semantics stage 2, docs/design/semantics-stage2.md §3.5).
//! `velt_rt` reads and writes values in their unboxed ("foreign") layout: an array is its
//! `{ data, len, cap }` header, an object its fields. When a type in an extern signature
//! contains a boxed array or object, arguments are passed as an unboxed *view* (a bitwise copy
//! that owns nothing) and results are moved into fresh boxes.

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::{ice, Cx, FnLower};
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

impl Cx<'_> {
    /// The layout runtime externs use for `t` (as if no type were boxed).
    pub(super) fn foreign_ty(&mut self, t: TyId) -> Ty {
        if let Some(&f) = self.lay.foreign.get(&t) {
            return f;
        }
        let native = self.ty(t);
        let f = match self.kind(t) {
            TyKind::Array(e) => {
                if self.foreign_differs(e) {
                    ice("a runtime extern exchanges an array of boxed values");
                }
                Ty::Agg(self.array_agg())
            }
            TyKind::Adt(d, _)
                if matches!(self.hir.def(d), hir::Def::Adt(_)) && !self.is_class(t) =>
            {
                let fields = self.adt_field_tys(t);
                self.foreign_agg(t, fields, native)
            }
            TyKind::Tuple(es) => self.foreign_agg(t, es, native),
            TyKind::Option(e) => match self.foreign_differs(e) {
                true => {
                    let fe = self.foreign_ty(e);
                    Ty::Agg(self.new_agg("foreign option".into(), &[Ty::Bool, fe]))
                }
                false => native,
            },
            _ => native,
        };
        self.lay.foreign.insert(t, f);
        f
    }

    /// Does the runtime see `t` differently from compiled code?
    pub(super) fn foreign_differs(&mut self, t: TyId) -> bool {
        let f = self.foreign_ty(t);
        f != self.ty(t)
    }

    /// The foreign layout of a struct / tuple with field types `fields`.
    fn foreign_agg(&mut self, t: TyId, fields: Vec<TyId>, native: Ty) -> Ty {
        let mut differs = self.boxed(t);
        let mut tys = vec![];
        for f in fields {
            let ft = self.foreign_ty(f);
            differs |= ft != self.ty(f);
            if ft != Ty::Unit {
                tys.push(ft);
            }
        }
        match differs {
            true => Ty::Agg(self.new_agg(format!("foreign {}", self.type_name(t)), &tys)),
            false => native,
        }
    }
}

impl FnLower<'_, '_> {
    /// An unboxed view of the native value at `src` (owning nothing) for a runtime extern.
    pub(super) fn foreign_view(&mut self, src: &Place, t: TyId) -> Place {
        if !self.cx.foreign_differs(t) {
            return src.clone();
        }
        let ft = self.cx.foreign_ty(t);
        let out = Place::local(self.temp(ft));
        match self.cx.kind(t) {
            TyKind::Array(_) => {
                let hdr = self.content(src, t);
                self.assign(out.clone(), Rvalue::Use(Operand::Copy(hdr)));
            }
            TyKind::Option(e) => {
                let zero = self.zero_value(ft);
                self.assign(out.clone(), Rvalue::Use(zero));
                let done = self.new_block();
                let some = self.option_is_some(src, t);
                self.when(some, done);
                let payload = self.some_payload(src, t);
                let v = self.foreign_view(&payload, e);
                self.assign(proj(&out, Proj::Field(0)), Rvalue::Use(Self::ctrue()));
                self.assign(proj(&out, Proj::Field(1)), Rvalue::Use(Operand::Copy(v)));
                self.goto(done);
                self.switch_to(done);
            }
            _ => {
                for (k, (i, f)) in self.stored_parts(t).into_iter().enumerate() {
                    let fp = self.field_place(src, t, i);
                    let v = self.foreign_view(&fp, f);
                    self.assign(
                        proj(&out, Proj::Field(k as u32)),
                        Rvalue::Use(Operand::Copy(v)),
                    );
                }
            }
        }
        out
    }

    /// Move the foreign value at `src` (written by a runtime extern) into the native place
    /// `dst` (uninitialized), boxing what is boxed.
    pub(super) fn adopt_foreign(&mut self, src: &Place, t: TyId, dst: &Place) {
        if !self.cx.foreign_differs(t) {
            self.assign(dst.clone(), Rvalue::Use(Operand::Copy(src.clone())));
            return;
        }
        match self.cx.kind(t) {
            TyKind::Array(_) => {
                let v = self.box_value(Operand::Copy(src.clone()), t);
                self.assign(dst.clone(), Rvalue::Use(v));
            }
            TyKind::Option(e) => {
                let none = self.none_value(t);
                self.assign(dst.clone(), Rvalue::Use(none));
                let done = self.new_block();
                let some = self.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Use(Operand::Copy(proj(src, Proj::Field(0)))),
                );
                self.when(some, done);
                let et = self.cx.ty(e);
                let v = Place::local(self.temp(et));
                self.adopt_foreign(&proj(src, Proj::Field(1)), e, &v);
                let ot = self.cx.ty(t);
                self.set_some(dst.clone(), ot, Operand::Copy(v));
                self.goto(done);
                self.switch_to(done);
            }
            _ => self.object_adopt_foreign(src, t, dst),
        }
    }

    /// A struct / tuple from its foreign layout, field by field (into a new box when boxed).
    fn object_adopt_foreign(&mut self, src: &Place, t: TyId, dst: &Place) {
        let inline = match self.cx.boxed(t) {
            true => {
                let payload = self.cx.payload_ty(t);
                let p = self.counted_alloc(payload);
                self.assign(dst.clone(), Rvalue::Use(p.clone()));
                let pp = self.operand_place(p, Ty::Ptr);
                proj(&pp, Proj::Deref(payload))
            }
            false => dst.clone(),
        };
        for (k, (i, f)) in self.stored_parts(t).into_iter().enumerate() {
            let field = Proj::Field(self.cx.vir_field(t, i));
            self.adopt_foreign(&proj(src, Proj::Field(k as u32)), f, &proj(&inline, field));
        }
    }

    /// `(field index, type)` of the fields of a struct / tuple that have storage.
    fn stored_parts(&mut self, t: TyId) -> Vec<(u32, TyId)> {
        let tys = self.cx.part_types(t);
        let mut out = vec![];
        for (i, f) in tys.into_iter().enumerate() {
            if !self.cx.is_unit(f) {
                out.push((i as u32, f));
            }
        }
        out
    }
}
