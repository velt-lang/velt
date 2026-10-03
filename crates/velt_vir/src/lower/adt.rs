//! Value construction: struct/anon/class literals, `new C(args)`, enum variants,
//! tuples, `Some` wrapping, interface values (`ToDyn`) and `shared(x)` boxes. Every constructed
//! value that owns resources is registered as an owned temporary.

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::{cint, ice, unit, FnLower, VtableKey};
use crate::vir::{Const, Operand, Place, Proj, Rvalue, Stmt, Ty};

impl FnLower<'_, '_> {
    pub(super) fn adt_lit(&mut self, ty: TyId, fields: &[hir::Expr]) -> Operand {
        let ty = self.sub(ty);
        if self.cx.is_class(ty) {
            let obj = self.alloc_object(ty);
            for (i, f) in fields.iter().enumerate() {
                let v = self.consume(f);
                let p = self.field_place(&obj, ty, i as u32);
                self.store(p, v);
            }
            return Operand::Copy(obj);
        }
        let ops: Vec<Operand> = fields.iter().map(|f| self.consume(f)).collect();
        self.build_agg(ty, ops)
    }

    pub(super) fn tuple(&mut self, es: &[hir::Expr], ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let ops: Vec<Operand> = es.iter().map(|e| self.consume(e)).collect();
        self.build_agg(ty, ops)
    }

    /// Aggregate value of concrete type `ty` from owned field operands (registered as a temp).
    fn build_agg(&mut self, ty: TyId, ops: Vec<Operand>) -> Operand {
        let Ty::Agg(a) = (match self.cx.boxed(ty) {
            true => self.cx.payload_ty(ty),
            false => self.cx.ty(ty),
        }) else {
            ice("aggregate literal of a non-aggregate type")
        };
        if self.dead() {
            return unit();
        }
        let ops = self.stored_fields(ty, ops);
        let t = self.temp(Ty::Agg(a));
        self.assign(Place::local(t), Rvalue::Aggregate(a, ops));
        if self.cx.boxed(ty) {
            let v = self.box_value(Operand::Copy(Place::local(t)), ty);
            return self.own_value(v, ty);
        }
        self.own_temp(t, ty);
        Operand::Copy(Place::local(t))
    }

    /// The field operands of a struct/tuple literal that have storage (`void` fields have none).
    fn stored_fields(&mut self, ty: TyId, ops: Vec<Operand>) -> Vec<Operand> {
        if !matches!(self.cx.kind(ty), TyKind::Adt(..) | TyKind::Tuple(_)) {
            return ops;
        }
        let tys = self.cx.part_types(ty);
        ops.into_iter()
            .zip(tys)
            .filter(|&(_, t)| !self.cx.is_unit(t))
            .map(|(o, _)| o)
            .collect()
    }

    /// Heap-allocate a zeroed object of class `ty` with its vtable pointer set; the object is
    /// registered as an owned temporary (so an error thrown by the constructor frees it).
    pub(super) fn alloc_object(&mut self, ty: TyId) -> Place {
        let obj = self.alloc_object_raw(ty);
        self.own_temp(obj.local, ty);
        obj
    }

    /// A zeroed object of class `ty` with its vtable pointer set, owned by the caller.
    pub(super) fn alloc_object_raw(&mut self, ty: TyId) -> Place {
        let oa = self.cx.obj_agg(ty);
        let size = self.cx.size_align(Ty::Agg(oa)).0;
        let ptr = self.object_alloc(ty);
        self.mem_set(ptr.clone(), cint(0, Ty::U8), cint(size as i128, Ty::U64));
        let obj = self.temp(Ty::Ptr);
        self.assign(Place::local(obj), Rvalue::Use(ptr));
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("object of a non-class type")
        };
        if self.cx.has_header(d) {
            let vt = self.vtable_addr(VtableKey::Class(ty));
            let hdr = proj(
                &proj(&Place::local(obj), Proj::Deref(Ty::Agg(oa))),
                Proj::Field(0),
            );
            self.assign(hdr, Rvalue::Use(vt));
        }
        Place::local(obj)
    }

    pub(super) fn variant(&mut self, ty: TyId, variant: u32, args: &[hir::Expr]) -> Operand {
        let ty = self.sub(ty);
        let tag_ty = match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_c_like_enum(d) => {
                let disc = self.cx.enum_def(d).variants[variant as usize].discriminant;
                return cint(disc as i128, Ty::I64);
            }
            TyKind::Adt(..) => Ty::U32,
            k => ice(format_args!("variant of {k:?}")),
        };
        let vals: Vec<Operand> = args.iter().map(|a| self.consume(a)).collect();
        if self.dead() {
            return unit();
        }
        let base = self.cx.ty(ty);
        let view = self.cx.view(ty, variant);
        let t = self.temp(base);
        let vp = proj(&Place::local(t), Proj::Cast(view));
        self.assign(
            proj(&vp, Proj::Field(0)),
            Rvalue::Use(cint(variant as i128, tag_ty)),
        );
        let mut k = 1;
        for v in vals {
            if !matches!(v, Operand::Const(Const::Unit, _)) {
                self.assign(proj(&vp, Proj::Field(k)), Rvalue::Use(v));
                k += 1;
            }
        }
        self.own_temp(t, ty);
        Operand::Copy(Place::local(t))
    }

    pub(super) fn wrap_some(&mut self, inner: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let v = self.consume(inner);
        match self.cx.ty(ty) {
            Ty::Ptr => self.own_value(v, ty),
            Ty::Bool => Operand::Const(Const::Bool(true), Ty::Bool),
            _ => self.build_agg(ty, vec![Operand::Const(Const::Bool(true), Ty::Bool), v]),
        }
    }

    /// Concrete value → interface value `{ data, vtable }`. Class objects and boxed values are
    /// their own data pointer; other values are moved into a heap box.
    pub(super) fn make_dyn(&mut self, e: &hir::Expr, impl_index: u32, ty: TyId) -> Operand {
        let cty = self.sub(e.ty);
        let ty = self.sub(ty);
        self.cx.note_dyn_identity(ty, cty);
        let v = self.consume(e);
        if self.dead() {
            return unit();
        }
        let data = if self.cx.is_class(cty) || self.cx.boxed(cty) {
            v
        } else {
            let vt = self.cx.ty(cty);
            let b = self.alloc(vt);
            let bp = self.operand_place(b.clone(), Ty::Ptr);
            self.store(proj(&bp, Proj::Deref(vt)), v);
            b
        };
        let vtable = self.vtable_addr(VtableKey::Impl(impl_index, cty));
        self.build_agg(ty, vec![data, vtable])
    }

    /// `shared(x)`: `{ count: 1, value: x }` on the heap.
    pub(super) fn shared_new(&mut self, arg: &hir::Expr, ty: TyId) -> Operand {
        let inner = self.sub(arg.ty);
        let v = self.consume(arg);
        let bx = self.cx.shared_box(inner);
        let p = self.alloc(Ty::Agg(bx));
        let bp = proj(
            &self.operand_place(p.clone(), Ty::Ptr),
            Proj::Deref(Ty::Agg(bx)),
        );
        self.assign(proj(&bp, Proj::Field(0)), Rvalue::Use(cint(1, Ty::U64)));
        self.store(proj(&bp, Proj::Field(1)), v);
        let ty = self.sub(ty);
        self.own_value(p, ty)
    }

    /// Copy `len` (u64) bytes between pointers (`overlapping`: memmove).
    pub(super) fn mem_copy_dyn(
        &mut self,
        dst: Operand,
        src: Operand,
        len: Operand,
        overlapping: bool,
    ) {
        self.push_stmt(Stmt::MemCopyDyn {
            dst,
            src,
            len,
            overlapping,
        });
    }

    /// Fill `len` (u64) bytes at pointer `dst` with `byte` (u8).
    pub(super) fn mem_set(&mut self, dst: Operand, byte: Operand, len: Operand) {
        self.push_stmt(Stmt::MemSet { dst, byte, len });
    }
}
