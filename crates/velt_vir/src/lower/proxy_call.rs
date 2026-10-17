//! A function-typed field called on a typed proxy (issue #823; hir_encodings.md "Proxy and
//! Reflect"): `s.$set(v)` where `$set` is a field of function type of the proxiable `S`. On a
//! proxy, JavaScript reads `$set` through the `get` trap (before the arguments), then calls
//! what it answered. The trap's answer is usually a function value of another type (sigx's
//! `$set` closure takes `any`), so the call goes through that value's `AnyApply` slot with the
//! arguments boxed, and the result is checked as the field's result type. A plain value calls
//! the field as before.

use velt_sema::hir::{self, TyId, TyKind};

use super::any_member::SLOT_APPLY;
use super::glue::Glue;
use super::operand::proj;
use super::{cint, FnLower, Work};
use crate::vir::{self, Callee, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `base.f(args)` where `base` has the proxiable type `bty` and field `index` is a function
    /// (module docs); `None` when this form does not apply (the caller lowers the call as usual).
    pub(super) fn proxy_field_call(
        &mut self,
        f: &hir::Expr,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Option<Operand> {
        let hir::ExprKind::Field { base, index, .. } = &f.kind else {
            return None;
        };
        let bty = self.sub(base.ty);
        if !self.cx.proxiable(bty) || self.cx.is_class(bty) {
            return None;
        }
        let (name, fty) = self.proxy_field(bty, *index);
        let TyKind::FnPtr {
            params,
            ret,
            throws,
        } = self.cx.kind(fty)
        else {
            return None;
        };
        let simple = params.iter().all(|&p| self.cx.ty(p) != Ty::Unit);
        if self.cx.error_ty(Some(throws)).is_some() || !simple || !self.cx.any_boxable(ret) {
            return None;
        }
        let ret = self.sub(ty);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let ca = self.cx.closure_agg();
        let v = self.expr(base);
        let bp = self.place_of(v, bty);
        let hop = self.retained_hop(&bp, bty);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(hop)));
        let tagged = self.proxy_tagged(ptr.clone());
        // The function: the trap's answer (an `any`, owned) or the field's value (a share).
        let fa = Place::local(self.temp(avt));
        let fv = Place::local(self.temp(Ty::Agg(ca)));
        // Both are written on one path of the first branch and read on the same path of the
        // second; zero first, so each is written on every path.
        let (za, zf) = (self.zero_value(avt), self.zero_value(Ty::Agg(ca)));
        self.assign(fa.clone(), Rvalue::Use(za));
        self.assign(fv.clone(), Rvalue::Use(zf));
        let (cold, hot, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(tagged.clone(), cold, hot);
        self.switch_to(cold);
        let u = self.proxy_untag(ptr.clone());
        let key = self.proxy_key_addr(&name);
        let out = self.addr(fa.clone());
        let g = self.cx.func(Work::Glue(Glue::ProxyGet, any));
        self.call(Callee::Func(g), vec![u, key, out], None, false);
        self.goto(join);
        self.switch_to(hot);
        let fp = self.proxy_hot_place(&ptr, bty, *index);
        let s = self.share_value(Operand::Copy(fp), fty);
        self.assign(fv.clone(), Rvalue::Use(s));
        self.goto(join);
        self.switch_to(join);
        let (argv, mut pts) = self.borrow_args(args, None, false, false);
        let rvt = self.cx.ty(ret);
        let res = (rvt != Ty::Unit).then(|| Place::local(self.temp(rvt)));
        let (cold, hot, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(tagged, cold, hot);
        self.switch_to(cold);
        self.proxy_apply_any(&fa, &argv, &params, ret, res.as_ref(), &name);
        self.goto(join);
        self.switch_to(hot);
        let code = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fv, Proj::Field(0)))),
        );
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&fv, Proj::Field(1)))),
        );
        let mut av = vec![env];
        av.extend(argv);
        pts.insert(0, Ty::Ptr);
        let abi = self.cx.ret_abi(ret, None);
        if abi.out.is_some() {
            pts.push(Ty::Ptr);
        }
        let callee = vir::Callee::Ptr {
            target: code,
            params: pts,
            ret: abi.ret,
        };
        let r = self.finish_call(callee, av, ret, None);
        if let Operand::Copy(p) = &r {
            self.take_temp(&p.clone());
        }
        if let Some(res) = &res {
            self.assign(res.clone(), Rvalue::Use(r));
        }
        self.drop_glue(fv, fty);
        self.goto(join);
        self.switch_to(join);
        Some(match res {
            Some(res) => self.own_value(Operand::Copy(res), ret),
            None => super::unit(),
        })
    }

    /// Call the function `any` at `fa` (owned; dropped here) through its `AnyApply` slot with
    /// the borrowed arguments `argv` (of types `params`) boxed; its result checked as a `ret`
    /// into `res`.
    fn proxy_apply_any(
        &mut self,
        fa: &Place,
        argv: &[Operand],
        params: &[TyId],
        ret: TyId,
        res: Option<&Place>,
        name: &str,
    ) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let buf = match params.is_empty() {
            true => cint(0, Ty::Ptr),
            false => {
                let tup = self.cx.intern(TyKind::Tuple(vec![any; params.len()]));
                let tvt = self.cx.ty(tup);
                let buf = Place::local(self.temp(tvt));
                for (k, (a, &p)) in argv.iter().zip(params).enumerate() {
                    let pv = match self.cx.ty(p) {
                        Ty::Agg(at) => {
                            let pp = self.operand_place(a.clone(), Ty::Ptr);
                            Operand::Copy(proj(&pp, Proj::Deref(Ty::Agg(at))))
                        }
                        _ => a.clone(),
                    };
                    let s = self.share_value(pv, p);
                    self.any_box_into(s, p, &proj(&buf, Proj::Field(k as u32)));
                }
                self.addr(buf)
            }
        };
        let out = Place::local(self.temp(avt));
        let oa = self.addr(out.clone());
        let data = Operand::Copy(proj(fa, Proj::Field(0)));
        let desc = Operand::Copy(proj(fa, Proj::Field(1)));
        let argc = cint(params.len() as i128, Ty::U64);
        let p = vec![Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr];
        let st = self.any_slot(desc, SLOT_APPLY, vec![data, buf, argc, oa], p);
        let what = format!("TypeError: calling '{name}' through a proxy is not supported for this value yet (#823)");
        self.any_status_check(st, (&what, None, ""), Some(name));
        self.drop_glue(fa.clone(), any);
        match res {
            None => self.drop_glue(out, any),
            Some(res) => {
                let v = self.any_unbox(&out, any, ret);
                self.assign(res.clone(), Rvalue::Use(v));
            }
        }
    }
}
