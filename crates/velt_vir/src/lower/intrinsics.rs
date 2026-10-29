//! Compiler intrinsics (`hir::Intrinsic`) mapped to inline code, glue or `velt_rt` calls.

use velt_sema::hir::{self, Intrinsic, TyId};

use super::async_fn::BoxHold;
use super::json::JsonSource;
use super::operand::proj;
use super::rt::Rt;
use super::{ice, unit, FnLower};
use crate::vir::{Operand, Place, Proj, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

impl FnLower<'_, '_> {
    pub(super) fn intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr], ty: TyId) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::Yield, [a, rest @ ..]) => self.yield_value(a, rest.first(), ty),
            (I::GeneratorResume, [g]) => self.gen_resume(g),
            (
                I::AtomicLoad
                | I::AtomicStore
                | I::AtomicRmw
                | I::AtomicCas
                | I::AtomicCasWeak
                | I::AtomicFence
                | I::AtomicSpinHint,
                _,
            ) => self.atomic_intrinsic(i, args),
            (I::ThreadLocalGet | I::ThreadLocalSet, _) => self.thread_local_intrinsic(i, args),
            (I::GeneratorResumeWith, [g, v]) => self.gen_resume_with(g, v),
            (I::GeneratorThrow, [g, e]) => self.gen_throw(g, e),
            (I::YieldDelegate, [g]) => self.yield_delegate(g),
            (I::GeneratorValue, [g]) => self.gen_value(g),
            (I::GeneratorReturn, [g]) => self.gen_return(g),
            (I::AsyncGeneratorValue, [g]) => self.gen_value(g),
            (I::AsyncGeneratorDispose, [g]) => self.gen_return(g),
            (I::AsyncGeneratorResume | I::AsyncGeneratorReturn, _) => {
                crate::lower::ice("async generator resume/return outside `await`")
            }
            (I::GeneratorEmbed, [_]) => {
                let e = hir::Expr {
                    kind: hir::ExprKind::Call {
                        callee: hir::Callee::Intrinsic(i),
                        args: args.to_vec(),
                    },
                    ty,
                    span: args[0].span,
                };
                self.gen_embed_value(&e)
            }
            (I::InitSlots(from, to), []) => {
                self.init_slots(from, to);
                unit()
            }
            (I::Print, _) => self.print(1, args),
            (I::SymbolStatic, [_, _, _]) => self.symbol_static(args),
            (I::SymbolRegistry, []) => self.symbol_registry(),
            (I::SpreadObject, parts) => self.spread_object(parts, ty),
            (I::KeyRead, [object, key]) => self.key_read(object, key, ty),
            (I::KeyWrite, [object, key, value]) => self.key_write(object, key, value),
            (I::FieldAbsent, []) => {
                // Outside a literal (which keeps the flag clear, adt.rs) it is just `null`; an
                // absent `a?: any` holds the dynamic `undefined`.
                let t = self.sub(ty);
                match self.cx.hir.any_impl.filter(|_| self.cx.is_dynamic(t)) {
                    Some(any) => self.dynamic_undefined(any, t),
                    None => self.none_value(t),
                }
            }
            (I::FieldPresent, [f]) => {
                if let hir::ExprKind::Field { base, index, .. } = &f.kind {
                    let bt = self.sub(base.ty);
                    if self.cx.proxiable(bt) {
                        return self.proxy_field_present(base, bt, *index);
                    }
                }
                // Only read: no write barrier.
                let outer = std::mem::replace(&mut self.read_target, true);
                let p = self.place_expr(f);
                self.read_target = outer;
                match self.presence_place(f, &p) {
                    Some(fp) => Operand::Copy(fp),
                    None => ice("`FieldPresent` of a field without a presence flag"),
                }
            }
            (I::PrintErr, _) => self.print(2, args),
            (I::ToString, [a]) => self.stringify(a, ty, false),
            (I::ErrorText, [a]) => self.text_of(a, Self::push_error_text),
            (I::Inspect, [a]) => self.text_of(a, Self::format_top_inspect),
            (I::ThrowLoc, []) => {
                let (_, buf) = self.new_strbuf(0);
                self.push_throw_loc(&buf);
                self.finish_text(buf)
            }
            (I::TemplateToString, [a]) => self.stringify(a, ty, true),
            (I::StrConcat, [a, b]) => self.str_concat(a, b, ty),
            (I::StrLen, [a]) => {
                let v = self.projection_base(a, BoxHold::No);
                self.str_len(v)
            }
            (I::StrByteLen, [a]) => {
                let v = self.projection_base(a, BoxHold::No);
                let p = self.operand_place(v, Ty::Agg(STR_AGG));
                self.str_bytes(&p)
            }
            (I::StrByteAt, [s, i]) => {
                let v = self.projection_base(s, BoxHold::No);
                let i = self.expr(i);
                self.str_byte_at(v, i)
            }
            (I::StrLoadU8x16, [s, at]) => {
                let v = self.projection_base(s, BoxHold::No);
                let at = self.expr(at);
                self.str_load_u8x16(v, at, None)
            }
            (I::StrLoadPartialU8x16, [s, at, count]) => {
                let v = self.projection_base(s, BoxHold::No);
                let at = self.expr(at);
                let count = self.expr(count);
                self.str_load_u8x16(v, at, Some(count))
            }
            (I::StrCharCodeAt, [s, i]) => {
                let hold = match self.may_write(i) {
                    true => BoxHold::Yes,
                    false => BoxHold::No,
                };
                let v = self.projection_base(s, hold);
                let i = self.expr(i);
                self.str_char_code_at(v, i)
            }
            (I::Exit, [a]) => {
                let v = self.expr(a);
                let from = self.vty(a.ty);
                let v = self.cast_to(v, from, Ty::I32);
                self.call_rt(Rt::Exit, vec![v], None);
                unit()
            }
            (I::Panic, [a]) => {
                let v = self.expr(a);
                let mut p = self.operand_addr(v, STR);
                let tracked = self.panic_loc() == Some(super::srcloc::ARG_LOC);
                if tracked || !self.panic_suffix().is_empty() {
                    // The process ends right after: the concatenation is never freed.
                    let at = self.panic_at();
                    let str_ty = self.cx.str_ty();
                    let msg = self.concat(p, at, str_ty);
                    p = self.operand_addr(msg, STR);
                }
                self.call_rt(Rt::Panic, vec![p], None);
                unit()
            }
            (
                I::ArrayWithCapacity
                | I::ArrayLen
                | I::ArrayPush
                | I::ArrayPop
                | I::ArraySwap
                | I::ArrayRemove
                | I::ArrayTruncate
                | I::ArrayMove
                | I::ArraySetLen
                | I::ArrayReserve
                | I::ArrayPrefetch
                | I::ArrayRotateRight
                | I::ArrayRotateLeft
                | I::ArraySwapNested,
                [_, ..],
            ) => self.array_intrinsic(i, args, ty),
            // An ordering's `compareTo` call (`float_param_ordering` takes floats first).
            (I::ParamOrdering, [a]) => self.expr(a),
            (I::Clone, [a]) => {
                let v = self.expr(a);
                let t = self.sub(a.ty);
                let c = self.clone_value(v, t);
                self.own_value(c, t)
            }
            (I::Transfer, [a]) => {
                let v = self.consume(a);
                let t = self.sub(a.ty);
                let v = self.transfer_value(v, t);
                self.own_value(v, t)
            }
            (I::NeedsTransfer, [a]) => {
                let t = self.sub(a.ty);
                Operand::Const(crate::vir::Const::Bool(self.cx.holds_counted(t)), Ty::Bool)
            }
            (I::NeedsDrop, [a]) => {
                Operand::Const(crate::vir::Const::Bool(self.needs_drop(a.ty)), Ty::Bool)
            }
            (I::MayAlias, [a]) => {
                let t = self.sub(a.ty);
                Operand::Const(crate::vir::Const::Bool(!self.cx.unique_refs(t)), Ty::Bool)
            }
            (I::HoldsNull, [a]) => {
                let t = self.sub(a.ty);
                let holds = matches!(self.cx.kind(t), hir::TyKind::Option(..));
                Operand::Const(crate::vir::Const::Bool(holds), Ty::Bool)
            }
            (I::NullAs, []) => {
                let t = self.sub(ty);
                if matches!(self.cx.kind(t), hir::TyKind::Option(..)) {
                    return self.none_value(t);
                }
                // Behind a `HoldsNull` test that is false here: never runs.
                self.panic_msg("ICE: null as a type without null");
                let vt = self.cx.ty(t);
                self.zero_value(vt)
            }
            (I::FnCapturesNothing, [f]) => self.fn_captures_nothing(f),
            (I::SimdShuffle(simd, mask), [a, b]) => self.simd_shuffle(simd, &mask, a, b),
            (I::ClosureSibling(def), []) => self.closure_sibling(def, ty),
            (I::Truthy, [a]) => self.truthy_intrinsic(a),
            (I::IsNull, [a]) => self.null_intrinsic(a, false),
            (I::IsNullish, [a]) => self.null_intrinsic(a, true),
            (I::LooseEq, [a, b]) => self.loose_eq_intrinsic(a, b),
            (I::MayHoldPrimitive, [a]) => {
                let t = self.sub(a.ty);
                Operand::Const(crate::vir::Const::Bool(!self.cx.object_only(t)), Ty::Bool)
            }
            (I::HoldsDynamic, [a]) => {
                let t = self.sub(a.ty);
                Operand::Const(crate::vir::Const::Bool(self.cx.holds_dynamic(t)), Ty::Bool)
            }
            (I::HoldsDynamicOrClass, [a]) => {
                let t = self.sub(a.ty);
                let b = self.cx.holds_dynamic_or_class(t);
                Operand::Const(crate::vir::Const::Bool(b), Ty::Bool)
            }
            (I::AnyTo, [a]) => self.any_to(a, ty),
            (I::AnyTo, [a, text]) => {
                let text = match &text.kind {
                    hir::ExprKind::Lit(hir::Lit::Str(s)) => s.clone(),
                    _ => super::ice("AnyTo text is not a string literal"),
                };
                let outer = self.any_context.replace(text);
                let v = self.any_to(a, ty);
                self.any_context = outer;
                v
            }
            (I::AnyTypeof, [a]) => self.any_typeof(a),
            (I::AnyGet, [o, k]) => self.any_get(o, k, ty),
            (I::AnyIndex, [o, i]) => self.any_index(o, i, ty),
            (I::AnyCall, [o, k, text, rest @ ..]) => self.any_call(o, Some(k), text, rest, ty),
            (I::AnyApply, [f, text, rest @ ..]) => self.any_call(f, None, text, rest, ty),
            (I::AnySet, [o, k, v]) => self.any_set(o, k, v, false),
            (I::AnySetIndex, [o, i, v]) => self.any_set(o, i, v, true),
            (I::AnyHas, [o, k]) => self.any_has(o, k),
            (I::AnyDelete, [o, k]) => self.any_delete(o, k),
            (I::ProxyArrayCall, [o, op]) => {
                let t = self.sub(o.ty);
                let v = self.expr(o);
                let (data, _) = self.any_parts(v, t);
                let op = self.expr(op);
                self.rt_i64(Rt::ProxyArrayCall, vec![data, op])
            }
            (I::IsTypedProxy, [x]) => {
                let t = self.sub(x.ty);
                if !self.cx.proxiable(t) || self.cx.ty(t) != Ty::Ptr {
                    return crate::vir::Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
                }
                let v = self.expr(x);
                self.proxy_tagged(v)
            }
            (I::ProxyArrayHole, [o, i, op]) => {
                let t = self.sub(o.ty);
                let v = self.expr(o);
                let (data, _) = self.any_parts(v, t);
                let (i, op) = (self.expr(i), self.expr(op));
                self.rt_i64(Rt::ProxyArrayHole, vec![data, i, op])
            }
            (I::AnyIsArray, [a]) => self.any_is_array(a),
            (I::AnyKeys, [o]) => self.any_keys(o, ty),
            (I::AnyTruthy, [a]) => self.any_truthy(a),
            (I::AnyTag, [a]) => self.any_tag_value(a),
            (I::AnyToExact, [a]) => self.any_to_exact(a, ty),
            (I::AnyToNumber, [a]) => self.any_to_number(a),
            (I::AnyStrictEq, [a, b]) => self.any_eq(a, b, false),
            (I::AnyLooseEq, [a, b]) => self.any_eq(a, b, true),
            (I::Share, [a]) => {
                let v = self.expr(a);
                let t = self.sub(a.ty);
                // Another reference to a frozen value would count it: a copy (frozen.rs).
                let c = match self.frozen_root(a) {
                    true => {
                        self.note_frozen_copy(a);
                        self.clone_value(v, t)
                    }
                    false => self.share_value(v, t),
                };
                self.own_value(c, t)
            }
            (I::Eq, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.key_eq_values(&pa, &pb, t)
            }
            (I::DeepEq, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.deep_eq_values(&pa, &pb, t)
            }
            (I::Same, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.same_values(&pa, &pb, t)
            }
            (I::IncludesEq, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.includes_values(&pa, &pb, t)
            }
            (I::KeyEq, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.key_same_values(&pa, &pb, t)
            }
            (I::SameValue, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.same_value_values(&pa, &pb, t)
            }
            (I::Hash, [a]) => {
                let t = self.sub(a.ty);
                let v = self.expr(a);
                let p = self.place_of(v, t);
                self.hash_value(&p, t)
            }
            (I::Sqrt | I::Floor | I::Ceil | I::Round | I::Trunc | I::FAbs, [a]) => self.math(i, a),
            (I::SharedNew, [a]) => self.shared_new(a, ty),
            (
                I::Spawn
                | I::SpawnHandled
                | I::Sleep
                | I::YieldNow
                | I::PromiseAll
                | I::PromiseRace
                | I::PromiseAny
                | I::PerfNow
                | I::DateNow
                | I::SharedAdd
                | I::SharedGet
                | I::SharedRead
                | I::SharedSet
                | I::MutexNew
                | I::MutexWith
                | I::RwLockNew
                | I::LockRead
                | I::PromiseWiden
                | I::PromiseSame
                | I::ChanSend
                | I::ChanReceive
                | I::ChanTrySend
                | I::ChanTryReceive,
                _,
            ) => self.async_intrinsic(i, args, ty),
            (I::JsonStringify, [a]) => self.json_stringify(a, ty),
            (I::JsonParse, [a, flags, depth]) => {
                self.json_parse(a, flags, depth, JsonSource::Text, ty)
            }
            (I::JsonParseRows, [a, flags, depth]) => {
                self.json_parse(a, flags, depth, JsonSource::Rows, ty)
            }
            (I::JsonParseCursor, [a, flags, depth]) => {
                self.json_parse(a, flags, depth, JsonSource::Cursor, ty)
            }
            (I::DbBind, [sink, a]) => self.db_bind(sink, a),
            (I::ReadRows, [a]) => self.read_rows(a, true, ty),
            (I::ReadRow, [a]) => self.read_rows(a, false, ty),
            (I::HttpHandler, [f]) => self.http_handler(f, ty),
            (I::Attempt, [f]) => self.attempt(f, ty),
            (
                I::WeakMapSet
                | I::WeakMapGet
                | I::WeakMapHas
                | I::WeakMapDelete
                | I::WeakMapValue
                | I::WeakRefNew
                | I::WeakRefDeref,
                _,
            ) => self.weak_intrinsic(i, args, ty),
            (I::ArrayDataPtr, [xs]) => {
                let v = self.expr(xs);
                let aty = self.sub(xs.ty);
                let avt = self.cx.ty(aty);
                let p = self.operand_place(v, avt);
                let p = self.content(&p, aty);
                let data = Operand::Copy(proj(&p, Proj::Field(0)));
                self.cast_to(data, Ty::Ptr, Ty::U64)
            }
            _ => ice(format_args!(
                "intrinsic {i:?} called with {} arguments",
                args.len()
            )),
        }
    }

    /// Evaluate two operands to places (the first held if the second may change it).
    /// `Intrinsic::ErrorText` or `Intrinsic::Inspect` of `a`: what `push` appends for it to a
    /// new builder, as an owned string.
    fn text_of(&mut self, a: &hir::Expr, push: fn(&mut Self, &Operand, &Place, TyId)) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let p = self.place_of(v, t);
        let (_, buf) = self.new_strbuf(0);
        push(self, &buf, &p, t);
        self.finish_text(buf)
    }

    /// The string the builder `buf` holds (an owned string; the builder is consumed).
    fn finish_text(&mut self, buf: Operand) -> Operand {
        let text = self.temp(Ty::Agg(STR_AGG));
        let tp = self.addr(Place::local(text));
        self.call_rt(Rt::StrbufFinish, vec![buf, tp], None);
        Operand::Copy(Place::local(text))
    }

    pub(super) fn two_places(&mut self, a: &hir::Expr, b: &hir::Expr) -> (Place, Place) {
        let (ta, tb) = (self.sub(a.ty), self.sub(b.ty));
        let later = self.later_of(b);
        let va = self.expr_held(a, later);
        let vb = self.expr(b);
        (self.place_of(va, ta), self.place_of(vb, tb))
    }

    fn math(&mut self, i: Intrinsic, a: &hir::Expr) -> Operand {
        let r = match i {
            Intrinsic::Sqrt => Rt::Sqrt,
            Intrinsic::Floor => Rt::Floor,
            Intrinsic::Ceil => Rt::Ceil,
            Intrinsic::Round => Rt::Round,
            Intrinsic::Trunc => Rt::Trunc,
            _ => Rt::FAbs,
        };
        let v = self.expr(a);
        let from = self.vty(a.ty);
        let v = self.cast_to(v, from, Ty::F64);
        let d = self.temp(Ty::F64);
        self.call_rt(r, vec![v], Some(Place::local(d)));
        self.cast_to(Operand::Copy(Place::local(d)), Ty::F64, from)
    }

    /// The dynamic `undefined` as a value of the `any` type `t`: a `boolean | undefined`
    /// holding nothing, boxed by implementation `any` (as sema boxes `undefined`).
    pub(super) fn dynamic_undefined(&mut self, any: u32, t: TyId) -> Operand {
        let b = self.cx.intern(hir::TyKind::Bool);
        let opt = self
            .cx
            .intern(hir::TyKind::Option(b, hir::Nothing::Undefined));
        let e = hir::Expr {
            kind: hir::ExprKind::Lit(hir::Lit::Null),
            ty: opt,
            span: velt_common::Span::default(),
        };
        self.make_dyn(&e, any, t)
    }
}

impl FnLower<'_, '_> {
    /// A call of the runtime function `r`, which returns an `i64`.
    fn rt_i64(&mut self, r: Rt, args: Vec<Operand>) -> Operand {
        let d = self.temp(Ty::I64);
        self.call_rt(r, args, Some(Place::local(d)));
        Operand::Copy(Place::local(d))
    }
}
