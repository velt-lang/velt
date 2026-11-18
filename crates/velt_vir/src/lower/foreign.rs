//! Runtime externs and boxed types (semantics stage 2, docs/design/semantics-stage2.md §3.5).
//! `velt_rt` and native libraries read and write values in their unboxed ("foreign") layout: an
//! array is its `{ data, len, cap }` header with elements in their foreign layout, an object its
//! fields. When a type in an extern signature contains a boxed array or object, arguments are
//! passed as an unboxed *view* (a bitwise copy that owns nothing; an array whose elements differ
//! gets a temporary buffer of element views, freed when the extern returns, [`ForeignBuf`]) and
//! results are moved into fresh boxes (an array whose elements differ into a new native buffer,
//! the foreign one freed).
//! A synchronous extern gets an array whose elements need no converting in place instead: its
//! own header, which it may write (`foreign_args`, #1831).

use velt_sema::hir::{self, TyId, TyKind};

use super::cint;
use super::operand::proj;
use super::rt::Rt;
use super::{Cx, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// What an extern call does once the extern returned ([`FnLower::free_foreign_bufs`]).
pub(super) enum ForeignBuf {
    /// A view it was given that holds array buffers of its own (an array whose elements differ
    /// from their native layout, at any depth): freed, walking the view as it was built.
    Bufs { view: Place, ty: TyId },
    /// An array of a view type (array_view.rs) it was given in place (`foreign_args`): when the
    /// box holds a view, the mirror the extern may have changed is written back.
    Flush { array: Operand, ty: TyId },
}

impl ForeignBuf {
    pub(super) fn flush(array: Operand, ty: TyId) -> Self {
        ForeignBuf::Flush { array, ty }
    }
}

impl Cx<'_> {
    /// The layout runtime externs use for `t` (as if no type were boxed).
    pub(super) fn foreign_ty(&mut self, t: TyId) -> Ty {
        if let Some(&f) = self.lay.foreign.get(&t) {
            return f;
        }
        let native = self.ty(t);
        // A recursive type sees itself as native while its layout is computed.
        self.lay.foreign.insert(t, native);
        let f = match self.kind(t) {
            TyKind::Array(_) => Ty::Agg(self.array_agg()),
            TyKind::Adt(d, _)
                if matches!(self.hir.def(d), hir::Def::Adt(_)) && !self.is_class(t) =>
            {
                let fields = self.adt_field_tys(t);
                self.foreign_agg(t, fields, native)
            }
            TyKind::Tuple(es) => self.foreign_agg(t, es, native),
            TyKind::Option(e, _) => match self.foreign_differs(e) {
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

    /// Does a value of `t` need converting between compiled code and an extern: its foreign
    /// layout differs, or it is an array whose header is the same but whose elements differ?
    pub(super) fn foreign_differs(&mut self, t: TyId) -> bool {
        match self.kind(t) {
            TyKind::Array(e) => self.boxed(t) || self.foreign_differs(e),
            _ => {
                let f = self.foreign_ty(t);
                f != self.ty(t)
            }
        }
    }

    /// The foreign layout of a struct / tuple with field types `fields`.
    fn foreign_agg(&mut self, t: TyId, fields: Vec<TyId>, native: Ty) -> Ty {
        let mut differs = self.boxed(t);
        let mut tys = vec![];
        for f in fields {
            let ft = self.foreign_ty(f);
            // A field converted in place (an array of differing elements) makes this a foreign
            // aggregate of its own too, so its fields are addressed by their foreign index.
            differs |= ft != self.ty(f) || self.foreign_differs(f);
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
    /// An unboxed view of the native value at `src` (owning nothing) for a runtime extern; a
    /// view holding array buffers goes to `bufs`.
    pub(super) fn foreign_view(
        &mut self,
        src: &Place,
        t: TyId,
        bufs: &mut Vec<ForeignBuf>,
    ) -> Place {
        let view = self.view_of(src, t);
        if self.holds_bufs(t) {
            bufs.push(ForeignBuf::Bufs {
                view: view.clone(),
                ty: t,
            });
        }
        view
    }

    fn view_of(&mut self, src: &Place, t: TyId) -> Place {
        if !self.cx.foreign_differs(t) {
            return src.clone();
        }
        let ft = self.cx.foreign_ty(t);
        let out = Place::local(self.temp(ft));
        match self.cx.kind(t) {
            TyKind::Array(e) => {
                let hdr = self.content(src, t);
                if !self.cx.foreign_differs(e) {
                    self.assign(out.clone(), Rvalue::Use(Operand::Copy(hdr)));
                } else {
                    self.array_view(&hdr, e, &out);
                }
            }
            TyKind::Option(e, _) => {
                let zero = self.zero_value(ft);
                self.assign(out.clone(), Rvalue::Use(zero));
                let done = self.new_block();
                let some = self.option_is_some(src, t);
                self.when(some, done);
                let payload = self.some_payload(src, t);
                let v = self.view_of(&payload, e);
                self.assign(proj(&out, Proj::Field(0)), Rvalue::Use(Self::ctrue()));
                self.assign(proj(&out, Proj::Field(1)), Rvalue::Use(Operand::Copy(v)));
                self.goto(done);
                self.switch_to(done);
            }
            _ => {
                for (k, (i, f)) in self.stored_parts(t).into_iter().enumerate() {
                    let fp = self.field_place(src, t, i);
                    let v = self.view_of(&fp, f);
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
            TyKind::Array(e) => {
                let hdr = match self.cx.foreign_differs(e) {
                    true => Operand::Copy(Place::local(self.adopt_array(src, e))),
                    false => Operand::Copy(src.clone()),
                };
                let v = self.box_value(hdr, t);
                self.assign(dst.clone(), Rvalue::Use(v));
            }
            TyKind::Option(e, _) => {
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
                let p = self.counted_alloc_of(payload, t);
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

    /// (stride, align) of an element in its foreign layout.
    fn foreign_stride(&mut self, e: TyId) -> (u32, u32) {
        let fe = self.cx.foreign_ty(e);
        self.cx.size_align(fe)
    }

    /// Element `k` of the foreign array at `hdr` (elements of type `e`).
    fn foreign_elem(&mut self, hdr: &Place, k: Operand, e: TyId) -> Place {
        let (stride, _) = self.foreign_stride(e);
        let off = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, k, cint(stride as i128, Ty::U64)),
        );
        let data = Operand::Copy(proj(hdr, Proj::Field(0)));
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, off));
        let fe = self.cx.foreign_ty(e);
        let pp = self.operand_place(p, Ty::Ptr);
        proj(&pp, Proj::Deref(fe))
    }

    /// Writes into `out` a foreign array header whose new buffer holds a view of each element of
    /// the native array `hdr` (elements of type `e`).
    fn array_view(&mut self, hdr: &Place, e: TyId, out: &Place) {
        let (stride, align) = self.foreign_stride(e);
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(hdr, Proj::Field(1)))),
        );
        let size = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, len.clone(), cint(stride as i128, Ty::U64)),
        );
        let data = self.temp(Ty::Ptr);
        self.call_rt(
            Rt::Alloc,
            vec![size, cint(align as i128, Ty::U64)],
            Some(Place::local(data)),
        );
        let a = self.cx.array_agg();
        let data = Operand::Copy(Place::local(data));
        self.assign(
            out.clone(),
            Rvalue::Aggregate(a, vec![data, len.clone(), len.clone()]),
        );
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, len, |lw, k| {
            let sp = lw.elem_place(hdr, k.clone(), e);
            let v = lw.view_of(&sp, e);
            let dp = lw.foreign_elem(out, k, e);
            lw.assign(dp, Rvalue::Use(Operand::Copy(v)));
        });
    }

    /// Frees the array buffers of the view of type `t` at `view` (built by [`Self::view_of`]):
    /// each element's own first, then the array's; only what the view holds (an absent option
    /// holds nothing).
    fn free_view(&mut self, view: &Place, t: TyId) {
        if !self.holds_bufs(t) {
            return;
        }
        match self.cx.kind(t) {
            TyKind::Array(e) => {
                if self.holds_bufs(e) {
                    let len = self.rvalue_temp(
                        Ty::U64,
                        Rvalue::Use(Operand::Copy(proj(view, Proj::Field(1)))),
                    );
                    let k = self.temp(Ty::U64);
                    self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
                    self.count_loop(k, len, |lw, k| {
                        let ep = lw.foreign_elem(view, k, e);
                        lw.free_view(&ep, e);
                    });
                }
                let (stride, align) = self.foreign_stride(e);
                self.free_foreign(view, stride, align);
            }
            TyKind::Option(e, _) => {
                let done = self.new_block();
                let some = Operand::Copy(proj(view, Proj::Field(0)));
                self.when(some, done);
                self.free_view(&proj(view, Proj::Field(1)), e);
                self.goto(done);
                self.switch_to(done);
            }
            _ => {
                for (k, (_, f)) in self.stored_parts(t).into_iter().enumerate() {
                    self.free_view(&proj(view, Proj::Field(k as u32)), f);
                }
            }
        }
    }

    /// A new native array header (temp) holding the elements of the foreign array at `src`
    /// (elements of type `e`, written by an extern), each adopted; the foreign buffer is freed.
    fn adopt_array(&mut self, src: &Place, e: TyId) -> crate::vir::Local {
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(src, Proj::Field(1)))),
        );
        // Native code's length: the one array length that comes from outside (MAX_ARRAY_LEN).
        let (stride, _) = self.stride(e);
        self.check_len(len.clone(), stride);
        let nat = self.inline_array_with_len(len.clone(), e);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, len, |lw, k| {
            let fsrc = lw.foreign_elem(src, k.clone(), e);
            let dst = lw.elem_place(&Place::local(nat), k, e);
            lw.adopt_foreign(&fsrc, e, &dst);
        });
        let (stride, align) = self.foreign_stride(e);
        self.free_foreign(src, stride, align);
        nat
    }

    /// Frees the buffer of the foreign array header at `hdr` (`cap` elements of `stride`).
    fn free_foreign(&mut self, hdr: &Place, stride: u32, align: u32) {
        let cap = Operand::Copy(proj(hdr, Proj::Field(2)));
        let size = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, cap, cint(stride as i128, Ty::U64)),
        );
        let data = Operand::Copy(proj(hdr, Proj::Field(0)));
        self.call_rt(
            Rt::Free,
            vec![data, size, cint(align as i128, Ty::U64)],
            None,
        );
    }

    /// Does a view of `t` hold a buffer of its own: an array of differing elements, at any
    /// depth?
    fn holds_bufs(&mut self, t: TyId) -> bool {
        if !self.cx.foreign_differs(t) {
            return false;
        }
        match self.cx.kind(t) {
            TyKind::Array(e) => self.cx.foreign_differs(e),
            TyKind::Option(e, _) => self.holds_bufs(e),
            _ => self
                .stored_parts(t)
                .into_iter()
                .any(|(_, f)| self.holds_bufs(f)),
        }
    }

    /// Frees the buffers of the views an extern call was given (it returned).
    pub(super) fn free_foreign_bufs(&mut self, bufs: Vec<ForeignBuf>) {
        for b in bufs {
            match b {
                ForeignBuf::Bufs { view, ty } => self.free_view(&view, ty),
                ForeignBuf::Flush { array, ty } => {
                    self.view_flush(array, ty, super::array_view::ViewFlush::All)
                }
            }
        }
    }
}
