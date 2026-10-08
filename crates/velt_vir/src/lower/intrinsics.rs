//! Compiler intrinsics (`hir::Intrinsic`) mapped to inline code, glue or `velt_rt` calls.

use velt_sema::hir::{self, Intrinsic, TyId};

use super::operand::proj;
use super::rt::Rt;
use super::sequence::Later;
use super::{ice, unit, FnLower};
use crate::vir::{Operand, Place, Proj, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

impl FnLower<'_, '_> {
    pub(super) fn intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr], ty: TyId) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::Yield, [a]) => self.yield_value(a),
            (I::GeneratorResume, [g]) => self.gen_resume(g),
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
            (I::Print, _) => self.print(1, args),
            (I::FieldAbsent, []) => {
                // Outside a literal (which keeps the flag clear, adt.rs) it is just `null`.
                let t = self.sub(ty);
                self.none_value(t)
            }
            (I::FieldPresent, [f]) => {
                let p = self.place_expr(f);
                match self.presence_place(f, &p) {
                    Some(fp) => Operand::Copy(fp),
                    None => ice("`FieldPresent` of a field without a presence flag"),
                }
            }
            (I::PrintErr, _) => self.print(2, args),
            (I::ToString, [a]) => self.stringify(a, ty),
            (I::StrConcat, [a, b]) => self.str_concat(a, b, ty),
            (I::StrLen, [a]) => {
                let v = self.expr(a);
                self.str_len(v)
            }
            (I::StrCharCodeAt, [s, i]) => {
                let v = self.expr(s);
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
                let suffix = self.panic_suffix();
                if !suffix.is_empty() {
                    // The process ends right after: the concatenation is never freed.
                    let at = self.str_lit(&suffix);
                    let at = self.operand_addr(at, STR);
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
                | I::ArraySetLen,
                [_, ..],
            ) => self.array_intrinsic(i, args, ty),
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
            (I::FnCapturesNothing, [f]) => self.fn_captures_nothing(f),
            (I::Share, [a]) => {
                let v = self.expr(a);
                let t = self.sub(a.ty);
                let c = self.share_value(v, t);
                self.own_value(c, t)
            }
            (I::Eq, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.key_eq_values(&pa, &pb, t)
            }
            (I::Same, [a, b]) => {
                let t = self.sub(a.ty);
                let (pa, pb) = self.two_places(a, b);
                self.same_values(&pa, &pb, t)
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
                | I::SharedSet
                | I::MutexNew
                | I::MutexWith
                | I::PromiseWiden
                | I::ChanSend
                | I::ChanReceive
                | I::ChanTrySend
                | I::ChanTryReceive,
                _,
            ) => self.async_intrinsic(i, args, ty),
            (I::JsonStringify, [a]) => self.json_stringify(a, ty),
            (I::JsonParse, [a, flags, depth]) => self.json_parse(a, flags, depth, ty),
            (I::HttpHandler, [f]) => self.http_handler(f, ty),
            (I::Attempt, [f]) => self.attempt(f, ty),
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
    pub(super) fn two_places(&mut self, a: &hir::Expr, b: &hir::Expr) -> (Place, Place) {
        let (ta, tb) = (self.sub(a.ty), self.sub(b.ty));
        let va = self.expr_held(a, Later::of(b));
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
}
