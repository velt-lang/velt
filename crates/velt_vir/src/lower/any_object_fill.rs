//! `AnyTo` an object type from an `any` holding an object of another object type (#1675):
//! `const e: G = { name: "e" } as any` with `interface G { name: string; tag?: string }`. Node
//! keeps the object and reads `e.tag` as `undefined`; Velt's object types have fixed layouts,
//! so in a program without proxies (with them it is a view, any_view.rs) the target is built
//! from the object's members, each read as through `any` and converted as `AnyTo` converts it:
//! an optional member it lacks is absent, a required one a `TypeError` (the conversion of
//! `undefined`). The result is a new object: a later write to one is not seen through the other.

use velt_sema::hir::{self, TyId};

use super::any::TAG_OBJECT;
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, BlockId, Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Where the `any` at `p` (of type `from`, owned) holds an object and `ty` is an object type
    /// (not a class): `ty` built from its members (module docs), then on to `done` with it in
    /// `out`. Any other value goes on in the current block.
    pub(super) fn any_object_fill(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        out: &Place,
        done: BlockId,
    ) {
        let Some((d, _)) = self.plain_adt(ty) else {
            return;
        };
        let a = self.cx.adt_def(d);
        // With proxies the object becomes a view instead (any_view.rs), and a trap's error
        // would need a route out of the conversion.
        if self.cx.hir.dyn_members.is_none()
            || self.cx.has_proxies()
            || self.record_fill
            || a.kind != hir::AdtKind::Anon
            || a.fields.is_empty()
            || a.fields.iter().any(|f| f.name.starts_with('#'))
            || self.prelude_record(ty).is_some()
        {
            return;
        }
        let fields: Vec<(String, bool)> = a
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.optional))
            .collect();
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let tag = self.any_tag_of(desc.clone());
        let obj = cint(TAG_OBJECT as i128, Ty::U64);
        let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, obj));
        let (fill, no) = (self.new_block(), self.new_block());
        self.branch(is_obj, fill, no);
        self.switch_to(fill);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let tys = self.cx.adt_field_tys(ty);
        let filling = std::mem::replace(&mut self.record_fill, true);
        // No route for a trap's error out of the conversion: in a program with proxies (known
        // from the second pass of lowering on) the fill is not made.
        let routes = std::mem::replace(&mut self.proxy_routes, false);
        let mut ops = vec![];
        let mut flags = vec![];
        for (i, ((name, optional), fty)) in fields.iter().zip(tys).enumerate() {
            let tmp = Place::local(self.temp(avt));
            let ta = self.addr(tmp.clone());
            let key = self.str_lit(name);
            let ka = self.operand_addr(key, Ty::Agg(crate::vir::STR_AGG));
            self.any_get_str(&data, &desc, ka, ta);
            if !optional {
                let v = self.any_unbox(&tmp, any, fty);
                if !self.cx.is_unit(fty) {
                    ops.push(v);
                }
                continue;
            }
            // An optional member: absent while the object's reads as `undefined`.
            let vt = self.cx.ty(fty);
            let (val, flag) = (
                Place::local(self.temp(vt)),
                Place::local(self.temp(Ty::Bool)),
            );
            let t = self.any_tag_of(Operand::Copy(proj(&tmp, Proj::Field(1))));
            let undef = cint(super::any::TAG_UNDEFINED as i128, Ty::U64);
            let missing = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, t, undef));
            let (absent, present, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(missing, absent, present);
            self.switch_to(absent);
            self.drop_glue(tmp.clone(), any);
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
            self.switch_to(present);
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
            if self.cx.presence_slot(ty, i as u32).is_some() {
                flags.push(Operand::Copy(flag));
            }
        }
        self.record_fill = filling;
        self.proxy_routes = routes;
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
}
