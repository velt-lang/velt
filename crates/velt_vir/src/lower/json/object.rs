//! JSON object decoding into structs, anon objects and classes: keys are matched against the
//! field names (unknown keys are skipped), a repeated key replaces the earlier value (like
//! `JSON.parse`), a missing field fails with `expected field "<name>"` unless its type is
//! `T | null` (absent and `null` are the same to a typed decode; only `JsonValue` tells them
//! apart with `has(key)` / `isNull()`): it stays null. A class object is allocated zeroed up front, so a failure
//! drops exactly what was decoded.

use velt_sema::hir::{TyId, TyKind};

use super::{Seg, STR};
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower};
use crate::vir::{BinOp, BlockId, Local, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    pub(super) fn json_read_object(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        ty: TyId,
        fail: BlockId,
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("object of a non-ADT type")
        };
        let tys = self.cx.adt_field_tys(ty);
        let fields: Vec<(String, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .zip(&tys)
            .map(|(f, t)| {
                (
                    f.name.clone(),
                    matches!(self.cx.kind(*t), TyKind::Option(_)),
                )
            })
            .collect();
        if fields.len() > 64 {
            ice("JSON.parse supports at most 64 fields per object");
        }
        if self.cx.is_class(ty) {
            let obj = self.alloc_object_raw(ty);
            self.assign(place.clone(), Rvalue::Use(Operand::Copy(obj)));
        } else {
            self.json_null_fields(place, ty, &fields, &tys);
        }
        let ro = Operand::Copy(Place::local(r));
        self.json_expect(Rt::JsonObjectStart, vec![ro.clone()], ctx, "object", fail);
        let seen = self.temp(Ty::U64);
        self.assign(Place::local(seen), Rvalue::Use(cint(0, Ty::U64)));
        let key = self.temp(STR);
        let (head, dispatch, end, bad) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.goto(head);
        self.switch_to(head);
        let k = self.temp(Ty::U8);
        let ka = self.addr(Place::local(key));
        self.call_rt(Rt::JsonNextKey, vec![ro, ka], Some(Place::local(k)));
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(k)),
            cases: vec![(1, dispatch), (0, end)],
            default: bad,
        });
        self.switch_to(bad);
        self.json_fail(ctx, "object", fail);
        self.switch_to(dispatch);
        for (i, ((name, _), fty)) in fields.iter().zip(&tys).enumerate() {
            self.json_read_member(
                r,
                place,
                ctx,
                (ty, i as u32, name, *fty),
                (key, seen, head),
                fail,
            );
        }
        self.json_skip_member(r, ctx, key, head, fail);
        self.switch_to(end);
        for (i, (name, nullable)) in fields.iter().enumerate() {
            if !nullable {
                self.json_require(ctx, seen, i as u32, name, fail);
            }
        }
    }

    /// Start every `T | null` field of a struct / anon object out as `null` (all-zero), the
    /// value it keeps when the key is absent (a class object is allocated zeroed).
    fn json_null_fields(
        &mut self,
        place: &Place,
        ty: TyId,
        fields: &[(String, bool)],
        tys: &[TyId],
    ) {
        for (i, ((_, nullable), fty)) in fields.iter().zip(tys).enumerate() {
            if *nullable {
                let fp = self.field_place(place, ty, i as u32);
                let vt = self.cx.ty(*fty);
                let size = self.cx.size_align(vt).0;
                let a = self.addr(fp);
                self.mem_set(a, cint(0, Ty::U8), cint(size as i128, Ty::U64));
            }
        }
    }

    /// `if (key == "<name>") { decode the field; seen |= bit; continue at head }`.
    fn json_read_member(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        (ty, i, name, fty): (TyId, u32, &str, TyId),
        (key, seen, head): (Local, Local, BlockId),
        fail: BlockId,
    ) {
        let ka = self.addr(Place::local(key));
        let lit = self.str_lit(name);
        let la = self.operand_addr(lit, STR);
        let eq = self.rt_u8(Rt::StrEq, vec![ka.clone(), la]);
        let (hit, miss) = (self.new_block(), self.new_block());
        self.branch(eq, hit, miss);
        self.switch_to(hit);
        let fp = self.field_place(place, ty, i);
        // A repeated key: the later value wins.
        self.drop_glue(fp.clone(), fty);
        self.json_zero(&fp, fty);
        let field_fail = self.new_block();
        self.json_read(r, &fp, ctx, fty, field_fail);
        let bit = cint(1i128 << i, Ty::U64);
        let s = Operand::Copy(Place::local(seen));
        self.assign(Place::local(seen), Rvalue::Binary(BinOp::BitOr, s, bit));
        self.call_rt(Rt::StrDrop, vec![ka.clone()], None);
        self.goto(head);
        self.switch_to(field_fail);
        self.call_rt(Rt::StrDrop, vec![ka], None);
        self.json_prepend(ctx, Seg::Field(name));
        self.goto(fail);
        self.switch_to(miss);
    }

    /// An unknown key: skip its value (a malformed one fails at `.<key>`).
    fn json_skip_member(&mut self, r: Local, ctx: Local, key: Local, head: BlockId, fail: BlockId) {
        let ok = self.rt_u8(Rt::JsonSkipValue, vec![Operand::Copy(Place::local(r))]);
        let (next, bad) = (self.new_block(), self.new_block());
        let ka = self.addr(Place::local(key));
        self.branch(ok, next, bad);
        self.switch_to(bad);
        self.json_prepend(ctx, Seg::Key(ka.clone()));
        self.call_rt(Rt::StrDrop, vec![ka.clone()], None);
        self.json_fail(ctx, "value", fail);
        self.switch_to(next);
        self.call_rt(Rt::StrDrop, vec![ka], None);
        self.goto(head);
    }

    /// Fail with `expected field "<name>"` unless bit `i` of `seen` is set.
    fn json_require(&mut self, ctx: Local, seen: Local, i: u32, name: &str, fail: BlockId) {
        let s = Operand::Copy(Place::local(seen));
        let bit = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, s, cint(1i128 << i, Ty::U64)),
        );
        let has = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, cint(0, Ty::U64)));
        let (next, missing) = (self.new_block(), self.new_block());
        self.branch(has, next, missing);
        self.switch_to(missing);
        self.json_fail(ctx, &format!("field \"{name}\""), fail);
        self.switch_to(next);
    }
}
