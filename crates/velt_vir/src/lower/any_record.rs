//! `AnyTo` of an `any` to a string literal type (a key known per instantiation), checked by the
//! string's content, and of an `any` holding a `Record<string, any>` to an object type: the
//! object built from the record's entries (`{} as Pick<T, K>`, docs/internals/design/
//! deferred-types.md, decision 5).

use velt_sema::hir::{self, DynMemberKind, TyId, TyKind};

use super::glue::VtableKey;
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The value of type `ty` the `any` at `p` (owned, consumed here) holds.
    /// The string literal type `ty` (`"a"`, a key known per instantiation) the `any` at `p`
    /// (owned, consumed here) holds: a string with that content (the box is released; the
    /// value is zero-sized), else a `TypeError` naming it.
    pub(super) fn any_unbox_str_literal(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        lit: &str,
    ) -> Operand {
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let str_ty = self.cx.str_ty();
        let want = self.vtable_addr(VtableKey::Impl(imp, str_ty));
        let is_str = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want));
        let (test, ok, bad) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_str, test, bad);
        self.switch_to(test);
        let s = self.str_lit(lit);
        let sa = self.operand_addr(s, Ty::Agg(crate::vir::STR_AGG));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let eq = self.str_eq(data, sa);
        self.branch(eq, ok, bad);
        self.switch_to(bad);
        self.any_type_error(desc, ty);
        self.switch_to(ok);
        self.drop_glue(p.clone(), from);
        super::unit()
    }

    /// The `Record<string, any>` type, if the program has the prelude's `Record`.
    pub(super) fn record_of_any(&mut self) -> Option<TyId> {
        let d = (0..self.cx.hir.defs.len() as u32)
            .map(hir::DefId)
            .find(|&d| {
                matches!(self.cx.hir.def(d), hir::Def::Adt(a)
                if (a.name == "Record" || a.name.ends_with("::Record"))
                    && a.fields.first().is_some_and(|f| f.name == "entries"))
            })?;
        let (str_ty, any) = (self.cx.str_ty(), self.cx.any_ty());
        Some(self.cx.intern(TyKind::Adt(d, vec![str_ty, any])))
    }

    /// Where the `any` at `p` (of type `from`, owned) holds a `Record<string, any>` and `ty` is
    /// an object type without optional fields: `ty` built from the record's entries, each value
    /// converted as `AnyTo` converts it, a missing key a `TypeError` naming it
    /// (docs/internals/design/deferred-types.md, decision 5); then on to `done` with it in
    /// `out`. Any other value goes on in the current block. Nothing for other target types.
    pub(super) fn any_record_fill(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        out: &Place,
        done: crate::vir::BlockId,
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return;
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            return;
        };
        if !self.cx.hir.record_fills
            || self.record_fill
            || self.cx.is_class(ty)
            || a.fields.is_empty()
        {
            return;
        }
        let names: Vec<String> = a.fields.iter().map(|f| f.name.clone()).collect();
        let optional: Vec<bool> = a.fields.iter().map(|f| f.optional).collect();
        if a.name == "Record" || a.name.ends_with("::Record") {
            return;
        }
        let Some(rec) = self.record_of_any() else {
            return;
        };
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let want = self.vtable_addr(VtableKey::Impl(imp, rec));
        let is_rec = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want));
        let (fill, no) = (self.new_block(), self.new_block());
        self.branch(is_rec, fill, no);
        self.switch_to(fill);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let tys = self.cx.adt_field_tys(ty);
        let filling = std::mem::replace(&mut self.record_fill, true);
        let mut ops = vec![];
        let mut flags = vec![];
        // Whether the record has each field's key (`record_fill_order`).
        let mut present = vec![];
        for (i, (name, fty)) in names.iter().zip(tys).enumerate() {
            let tmp = Place::local(self.temp(avt));
            let ta = self.addr(tmp.clone());
            let key = self.str_lit(name);
            let ka = self.operand_addr(key, Ty::Agg(crate::vir::STR_AGG));
            self.any_get_str(&data, &desc, ka.clone(), ta);
            // Whether the record has the key (a key set to `undefined` is present).
            self.cx.note_any_has();
            let st = self.any_slot(
                desc.clone(),
                super::any_has::SLOT_HAS,
                vec![data.clone(), ka],
                vec![Ty::Ptr; 2],
            );
            let yes = cint(super::any_has::HAS_YES, Ty::U8);
            let missing = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, st, yes));
            let (absent, ok) = (self.new_block(), self.new_block());
            self.branch(missing, absent, ok);
            if !optional[i] {
                // A key `{} as D` never set: TypeScript would hand out an object without it.
                self.switch_to(absent);
                self.panic_msg(&format!(
                    "TypeError: the object built with `{{}} as` lacks its field `{name}`"
                ));
                self.terminate(crate::vir::Terminator::Unreachable);
                self.switch_to(ok);
                let v = self.any_unbox(&tmp, any, fty);
                if !self.cx.is_unit(fty) {
                    ops.push(v);
                }
                present.push(Operand::Const(Const::Bool(true), Ty::Bool));
                continue;
            }
            // An optional field: absent while the record lacks the key, else its value.
            let vt = self.cx.ty(fty);
            let (val, flag) = (
                Place::local(self.temp(vt)),
                Place::local(self.temp(Ty::Bool)),
            );
            let join = self.new_block();
            self.switch_to(absent);
            self.drop_glue(tmp.clone(), any);
            // An absent `a?: any` holds the dynamic `undefined`, any other the option's nothing.
            let none = match self.cx.hir.any_impl.filter(|_| self.cx.is_dynamic(fty)) {
                Some(imp) => self.dynamic_undefined(imp, fty),
                None => self.none_value(fty),
            };
            self.assign(val.clone(), Rvalue::Use(none));
            self.assign(
                flag.clone(),
                Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
            );
            self.goto(join);
            self.switch_to(ok);
            let v = self.any_unbox(&tmp, any, fty);
            self.assign(val.clone(), Rvalue::Use(v));
            self.assign(
                flag.clone(),
                Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
            );
            self.goto(join);
            self.switch_to(join);
            if !self.cx.is_unit(fty) {
                ops.push(Operand::Copy(val));
            }
            present.push(Operand::Copy(flag.clone()));
            if self.cx.presence_slot(ty, i as u32).is_some() {
                flags.push(Operand::Copy(flag));
            }
        }
        self.record_fill = filling;
        self.record_fill_order(&data, rec, &names, &present);
        ops.extend(flags);
        self.drop_glue(p.clone(), from);
        let agg = match self.cx.boxed(ty) {
            true => self.cx.payload_ty(ty),
            false => self.cx.ty(ty),
        };
        let Ty::Agg(agg) = agg else {
            super::ice("an object type that is no aggregate");
        };
        let t = self.temp(Ty::Agg(agg));
        self.assign(Place::local(t), Rvalue::Aggregate(agg, ops));
        let v = match self.cx.boxed(ty) {
            true => self.box_value(Operand::Copy(Place::local(t)), ty),
            false => Operand::Copy(Place::local(t)),
        };
        self.assign(out.clone(), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(no);
    }

    /// The record `data` holds (of type `rec`) has a key the object type with fields `names`
    /// lacks, or its keys in another order: Velt's object has its type's fields in their order,
    /// where JavaScript's object would have the record's (decision 5 in
    /// docs/internals/design/deferred-types.md), so it is a `TypeError`. The record's keys
    /// (`__keyNames`, in JavaScript's order) must be the names of the fields it holds
    /// (`present`), in order.
    fn record_fill_order(
        &mut self,
        data: &Operand,
        rec: TyId,
        names: &[String],
        present: &[Operand],
    ) {
        let key_names = self.plain_adt(rec).and_then(|(d, _)| {
            self.dyn_members_of(d)
                .into_iter()
                .find(|m| m.name == "__keyNames" && m.kind == DynMemberKind::Method)
        });
        let Some(r) = key_names.and_then(|m| self.ready(rec, &m)) else {
            return;
        };
        let dl = self.temp(Ty::Ptr);
        self.assign(Place::local(dl), Rvalue::Use(data.clone()));
        let this = self.any_this(dl, rec);
        let keys_v = self.invoke_ready(&r, this, vec![]);
        let keys_ty = r.ret;
        let kt = self.cx.ty(keys_ty);
        let keys = Place::local(self.temp(kt));
        self.assign(keys.clone(), Rvalue::Use(keys_v));
        let str_ = self.cx.str_ty();
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&keys, Proj::Field(1)))),
        );
        let j = Place::local(self.temp(Ty::U64));
        self.assign(j.clone(), Rvalue::Use(cint(0, Ty::U64)));
        let bad = self.new_block();
        for (name, has) in names.iter().zip(present) {
            let (check, next) = (self.new_block(), self.new_block());
            self.branch(has.clone(), check, next);
            self.switch_to(check);
            let jv = Operand::Copy(j.clone());
            let more =
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, jv.clone(), len.clone()));
            let at = self.new_block();
            self.branch(more, at, bad);
            self.switch_to(at);
            let key = self.elem_place(&keys, jv.clone(), str_);
            let lit = self.str_lit(name);
            let (ka, la) = (
                self.addr(key),
                self.operand_addr(lit, Ty::Agg(crate::vir::STR_AGG)),
            );
            let same = self.str_eq(ka, la);
            let step = self.new_block();
            self.branch(same, step, bad);
            self.switch_to(step);
            let j1 = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, jv, cint(1, Ty::U64)));
            self.assign(j.clone(), Rvalue::Use(j1));
            self.goto(next);
            self.switch_to(next);
        }
        let all = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, Operand::Copy(j), len));
        let ok = self.new_block();
        self.branch(all, ok, bad);
        self.switch_to(bad);
        self.panic_msg(
            "TypeError: the object built with `{} as` has keys of its type in another order, or keys its type lacks",
        );
        self.terminate(crate::vir::Terminator::Unreachable);
        self.switch_to(ok);
        self.drop_glue(keys, keys_ty);
    }
}
