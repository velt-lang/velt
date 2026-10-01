//! `JSON.parse` glue: the whole-document entry (`Glue::JsonParse`) and per-type decoders
//! (`Glue::JsonRead`; objects in object.rs).
//!
//! Decoder contract `(r, out, ctx) -> bool`: on success `*out` holds an owned value; on failure
//! `*out` is all-zero (owns nothing), `ctx.expected` names what was expected and `ctx.path` holds
//! the path *below* this value. Numbers, bools, strings and options are decoded inline; other
//! types call their glue.

use velt_sema::hir::{self, TyId, TyKind};

use super::{Seg, STR};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower, Glue};
use crate::vir::{BinOp, BlockId, Const, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// `Glue::JsonParse` body `(src, out, err) -> bool`: reader, decode, trailing-input check,
    /// error message (`velt_rt_json_error` with the `$`-rooted path).
    pub(in crate::lower) fn json_parse_body(
        &mut self,
        src: Local,
        out: Local,
        err: Local,
        ty: TyId,
    ) {
        let r = self.temp(Ty::Ptr);
        self.call_rt(
            Rt::JsonReaderNew,
            vec![Operand::Copy(Place::local(src))],
            Some(Place::local(r)),
        );
        let ca = self.cx.json_ctx_agg();
        let ctx = self.temp(Ty::Agg(ca));
        let (e0, e1) = (self.str_lit(""), self.str_lit(""));
        self.assign(Place::local(ctx), Rvalue::Aggregate(ca, vec![e0, e1]));
        let cp = self.addr(Place::local(ctx));
        let cpl = self.copy_to_temp(cp.clone(), Ty::Ptr);
        let ok = self.temp(Ty::Bool);
        let r_op = Operand::Copy(Place::local(r));
        let res = self.call_glue(
            Glue::JsonRead,
            ty,
            vec![r_op.clone(), Operand::Copy(Place::local(out)), cp],
        );
        self.assign(Place::local(ok), Rvalue::Use(res.clone()));
        let (end_bb, fail_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(res, end_bb, fail_bb);
        self.switch_to(end_bb);
        let ended = self.rt_u8(Rt::JsonEnd, vec![r_op.clone()]);
        let trailing = self.new_block();
        self.branch(ended, done, trailing);
        self.switch_to(trailing);
        let op = self.deref_param(out, ty);
        self.drop_glue(op, ty);
        self.json_fail(cpl, "end of input", fail_bb);
        self.switch_to(fail_bb);
        self.assign(
            Place::local(ok),
            Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
        );
        self.json_error_message(r_op.clone(), ctx, err);
        self.goto(done);
        self.switch_to(done);
        let path = self.addr(proj(&Place::local(ctx), Proj::Field(1)));
        self.call_rt(Rt::StrDrop, vec![path], None);
        self.call_rt(Rt::JsonReaderFree, vec![r_op], None);
        self.terminate(Terminator::Return(Operand::Copy(Place::local(ok))));
    }

    /// `*err = velt_rt_json_error(r, ctx.expected, "$" + ctx.path)`.
    fn json_error_message(&mut self, r: Operand, ctx: Local, err: Local) {
        let full = self.temp(STR);
        let fa = self.addr(Place::local(full));
        self.call_rt(Rt::StrbufNew, vec![cint(0, Ty::U64), fa.clone()], None);
        self.push_text(&fa, "$");
        let path = self.addr(proj(&Place::local(ctx), Proj::Field(1)));
        self.call_rt(Rt::StrbufPushStr, vec![fa.clone(), path], None);
        let expected = self.addr(proj(&Place::local(ctx), Proj::Field(0)));
        let args = vec![r, expected, fa.clone(), Operand::Copy(Place::local(err))];
        self.call_rt(Rt::JsonError, args, None);
        self.call_rt(Rt::StrDrop, vec![fa], None);
    }

    /// `Glue::JsonRead` body `(r, out, ctx) -> bool`.
    pub(in crate::lower) fn json_read_body(&mut self, r: Local, out: Local, ctx: Local, ty: TyId) {
        let place = self.deref_param(out, ty);
        let fail = self.new_block();
        self.json_zero(&place, ty);
        self.json_read_expand(r, &place, ctx, ty, fail);
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(true),
            Ty::Bool,
        )));
        self.switch_to(fail);
        self.drop_glue(place.clone(), ty);
        self.json_zero(&place, ty);
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(false),
            Ty::Bool,
        )));
    }

    /// Reset a droppable value's memory to all-zero ("owns nothing").
    pub(super) fn json_zero(&mut self, place: &Place, ty: TyId) {
        if self.cx.needs_drop(ty) {
            let vt = self.cx.ty(ty);
            let size = self.cx.size_align(vt).0;
            let a = self.addr(place.clone());
            self.mem_set(a, cint(0, Ty::U8), cint(size as i128, Ty::U64));
        }
    }

    pub(super) fn rt_u8(&mut self, r: Rt, args: Vec<Operand>) -> Operand {
        let d = self.temp(Ty::U8);
        self.call_rt(r, args, Some(Place::local(d)));
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(d)), cint(0, Ty::U8)),
        )
    }

    /// Continue if the rt call `r(args)` returned 1, else fail with `expected`.
    pub(super) fn json_expect(
        &mut self,
        rt: Rt,
        args: Vec<Operand>,
        ctx: Local,
        expected: &str,
        fail: BlockId,
    ) {
        let ok = self.rt_u8(rt, args);
        let (next, bad) = (self.new_block(), self.new_block());
        self.branch(ok, next, bad);
        self.switch_to(bad);
        self.json_fail(ctx, expected, fail);
        self.switch_to(next);
    }

    /// Numbers, bools, strings, `void`, options and C-like enums are decoded inline.
    fn json_read_inline(&mut self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Unit => true,
            TyKind::Option(_) => true,
            TyKind::Adt(d, _) => {
                matches!(self.cx.hir.def(d), hir::Def::Enum(_)) && self.cx.is_c_like_enum(d)
            }
            _ => false,
        }
    }

    /// Decode into `place` (all-zero before); on failure `place` is all-zero and control goes
    /// to `fail` with `ctx` set.
    pub(super) fn json_read(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        ty: TyId,
        fail: BlockId,
    ) {
        if self.json_read_inline(ty) {
            self.json_read_expand(r, place, ctx, ty, fail);
            return;
        }
        let a = self.addr(place.clone());
        let args = vec![
            Operand::Copy(Place::local(r)),
            a,
            Operand::Copy(Place::local(ctx)),
        ];
        let ok = self.call_glue(Glue::JsonRead, ty, args);
        let next = self.new_block();
        self.branch(ok, next, fail);
        self.switch_to(next);
    }

    fn json_read_expand(&mut self, r: Local, place: &Place, ctx: Local, ty: TyId, fail: BlockId) {
        let ro = Operand::Copy(Place::local(r));
        let vt = self.cx.ty(ty);
        if self.cx.boxed(ty) {
            // A boxed array / object is read into a fresh box of its own.
            let payload = self.cx.payload_ty(ty);
            let p = self.counted_alloc(payload);
            let pp = self.operand_place(p.clone(), Ty::Ptr);
            let zero = self.zero_value(payload);
            self.assign(
                crate::lower::operand::proj(&pp, crate::vir::Proj::Deref(payload)),
                Rvalue::Use(zero),
            );
            self.assign(place.clone(), Rvalue::Use(p));
        }
        match self.cx.kind(ty) {
            TyKind::Int(it) => self.json_read_int(r, place, ctx, it, fail),
            TyKind::Float(_) => {
                let t = self.temp(Ty::F64);
                let a = self.addr(Place::local(t));
                self.json_expect(Rt::JsonReadF64, vec![ro, a], ctx, "number", fail);
                let v = self.cast_to(Operand::Copy(Place::local(t)), Ty::F64, vt);
                self.assign(place.clone(), Rvalue::Use(v));
            }
            TyKind::Bool => {
                let t = self.temp(Ty::U8);
                let a = self.addr(Place::local(t));
                self.json_expect(Rt::JsonReadBool, vec![ro, a], ctx, "boolean", fail);
                let b = Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(t)), cint(0, Ty::U8));
                self.assign(place.clone(), b);
            }
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.json_expect(Rt::JsonReadString, vec![ro, a], ctx, "string", fail);
            }
            TyKind::Unit => self.json_expect(Rt::JsonReadNull, vec![ro], ctx, "null", fail),
            TyKind::Option(e) => self.json_read_option(r, place, ctx, ty, e, fail),
            TyKind::Array(e) => {
                let arr = self.content(place, ty);
                self.json_read_array(r, &arr, ctx, e, fail)
            }
            TyKind::Adt(d, _) if self.json_read_inline(ty) => {
                self.json_read_enum(r, place, ctx, d, fail)
            }
            TyKind::Adt(d, _) if matches!(self.cx.hir.def(d), hir::Def::Adt(_)) => {
                self.json_read_object(r, place, ctx, ty, fail)
            }
            k => ice(format_args!(
                "JSON.parse into a non-deserializable type {k:?}"
            )),
        }
    }

    /// Integers via `read_i64` (exact integers only), range-checked for narrower types.
    fn json_read_int(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        it: hir::IntTy,
        fail: BlockId,
    ) {
        let name = format!("{it:?}").to_lowercase();
        let t = self.temp(Ty::I64);
        let a = self.addr(Place::local(t));
        let ro = Operand::Copy(Place::local(r));
        self.json_expect(Rt::JsonReadI64, vec![ro, a], ctx, &name, fail);
        let vt = crate::lower::types::int_ty(it);
        let tv = Operand::Copy(Place::local(t));
        let c = self.cast_to(tv.clone(), Ty::I64, vt);
        if vt != Ty::I64 {
            let back = self.cast_to(c.clone(), vt, Ty::I64);
            let mut ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, tv.clone()));
            if !it.is_signed() {
                let nonneg =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ge, tv, cint(0, Ty::I64)));
                ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, ok, nonneg));
            }
            let (next, bad) = (self.new_block(), self.new_block());
            self.branch(ok, next, bad);
            self.switch_to(bad);
            self.json_fail(ctx, &name, fail);
            self.switch_to(next);
        }
        self.assign(place.clone(), Rvalue::Use(c));
    }

    /// `null` → none; anything else decodes the payload.
    fn json_read_option(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        ty: TyId,
        e: TyId,
        fail: BlockId,
    ) {
        let ro = Operand::Copy(Place::local(r));
        let kind = self.temp(Ty::U32);
        self.call_rt(Rt::JsonPeek, vec![ro.clone()], Some(Place::local(kind)));
        let is_null = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Eq,
                Operand::Copy(Place::local(kind)),
                cint(1, Ty::U32),
            ),
        );
        let (null_bb, some_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_null, null_bb, some_bb);
        self.switch_to(null_bb);
        self.json_expect(Rt::JsonReadNull, vec![ro], ctx, "null", fail);
        self.goto(done);
        self.switch_to(some_bb);
        match self.cx.ty(ty) {
            Ty::Agg(_) => {
                self.json_read(r, &proj(place, Proj::Field(1)), ctx, e, fail);
                self.assign(proj(place, Proj::Field(0)), Rvalue::Use(Self::ctrue()));
            }
            // Null niche: the payload is the pointer itself.
            _ => self.json_read(r, place, ctx, e, fail),
        }
        self.goto(done);
        self.switch_to(done);
    }

    /// `[a, b, …]` appended element by element (a failing element adds `[i]` to the path).
    fn json_read_array(&mut self, r: Local, place: &Place, ctx: Local, e: TyId, fail: BlockId) {
        let ro = Operand::Copy(Place::local(r));
        self.json_expect(Rt::JsonArrayStart, vec![ro.clone()], ctx, "array", fail);
        let i = self.temp(Ty::U64);
        self.assign(Place::local(i), Rvalue::Use(cint(0, Ty::U64)));
        let (head, body, done, bad) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.goto(head);
        self.switch_to(head);
        let k = self.temp(Ty::U8);
        self.call_rt(Rt::JsonArrayNext, vec![ro], Some(Place::local(k)));
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(k)),
            cases: vec![(1, body), (0, done)],
            default: bad,
        });
        self.switch_to(bad);
        self.json_fail(ctx, "array", fail);
        self.switch_to(body);
        let et = match self.cx.ty(e) {
            Ty::Unit => ice("JSON.parse of a void[]"),
            t => t,
        };
        let elem = self.temp(et);
        self.json_zero(&Place::local(elem), e);
        let elem_fail = self.new_block();
        self.json_read(r, &Place::local(elem), ctx, e, elem_fail);
        self.push_value(place, e, Operand::Copy(Place::local(elem)));
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(i)), cint(1, Ty::U64)),
        );
        self.assign(Place::local(i), Rvalue::Use(n));
        self.goto(head);
        self.switch_to(elem_fail);
        self.json_prepend(ctx, Seg::Index(Operand::Copy(Place::local(i))));
        self.goto(fail);
        self.switch_to(done);
    }

    /// A C-like enum from its discriminant number.
    fn json_read_enum(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        d: hir::DefId,
        fail: BlockId,
    ) {
        let t = self.temp(Ty::I64);
        let a = self.addr(Place::local(t));
        let ro = Operand::Copy(Place::local(r));
        self.json_expect(Rt::JsonReadI64, vec![ro, a], ctx, "enum", fail);
        let discs: Vec<i64> = self
            .cx
            .enum_def(d)
            .variants
            .iter()
            .map(|v| v.discriminant)
            .collect();
        let (ok, bad) = (self.new_block(), self.new_block());
        let cases = discs.into_iter().map(|v| (v as i128, ok)).collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(t)),
            cases,
            default: bad,
        });
        self.switch_to(bad);
        self.json_fail(ctx, "enum", fail);
        self.switch_to(ok);
        self.assign(place.clone(), Rvalue::Use(Operand::Copy(Place::local(t))));
    }
}
