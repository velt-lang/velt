//! JSON object decoding into structs, anon objects and classes: keys are matched against the
//! field names (unknown keys are skipped; no key fills a member a symbol names), a repeated key replaces the earlier value (like
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
                    matches!(self.cx.kind(*t), TyKind::Option(..)),
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
        // The key stays in the reader: fields compare its bytes in place (`keyIs`), and only an
        // error path makes it a string.
        let k = self.reader_call("nextKey", vec![ro]);
        self.terminate(Terminator::Switch {
            value: k,
            cases: vec![(1, dispatch), (0, end)],
            default: bad,
        });
        self.switch_to(bad);
        self.json_fail(ctx, "object", fail);
        self.switch_to(dispatch);
        for (i, ((name, _), fty)) in fields.iter().zip(&tys).enumerate() {
            // No key fills a member a symbol or a private name (`#x`) names, whatever its text
            // (`"[Symbol(k)]"` is a string key), as in JavaScript: it stays absent.
            if velt_sema::property_order::is_hidden_key(name) {
                continue;
            }
            self.json_read_member(
                r,
                place,
                ctx,
                (ty, i as u32, name, *fty),
                (seen, head),
                fail,
            );
        }
        self.json_skip_member(r, ctx, key, head, fail);
        self.switch_to(end);
        for (i, (name, nullable)) in fields.iter().enumerate() {
            // The prelude `Error`'s `#name` (the only private field sema lets decoding reach):
            // its default, as no key fills it.
            if name == "#name" && matches!(self.cx.kind(tys[i]), TyKind::Str) {
                let v = self.str_lit("Error");
                let f = self.field_place(place, ty, i as u32);
                self.assign(f, Rvalue::Use(v));
                continue;
            }
            if !nullable {
                // A required symbol-keyed member is never filled: the decode fails.
                let shown = velt_sema::property_order::symbol_key(name);
                self.json_require(ctx, seen, i as u32, shown.as_deref().unwrap_or(name), fail);
            }
            // `a?: T`: present when its key was there and its value is not the option's own
            // nothing (`undefined`): a JSON `null` for an `a?: T` leaves the field absent, and
            // one for an `a?: T | null` is a present `null`.
            if let Some(slot) = self.cx.presence_slot(ty, i as u32) {
                let fp = self.field_place(place, ty, i as u32);
                let mut f = fp.clone();
                if let Some(crate::vir::Proj::Field(x)) = f.proj.last_mut() {
                    *x = slot;
                }
                let s = Operand::Copy(Place::local(seen));
                let bit = self.rvalue_temp(
                    Ty::U64,
                    Rvalue::Binary(BinOp::BitAnd, s, cint(1i128 << i, Ty::U64)),
                );
                let seen_key =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, cint(0, Ty::U64)));
                let is_some = self.option_is_some(&fp, tys[i]);
                let set =
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, seen_key, is_some));
                self.assign(f, Rvalue::Use(set));
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

    /// `if (r.keyIs("<name>")) { decode the field; seen |= bit; continue at head }`.
    fn json_read_member(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        (ty, i, name, fty): (TyId, u32, &str, TyId),
        (seen, head): (Local, BlockId),
        fail: BlockId,
    ) {
        let (hit, miss) = (self.new_block(), self.new_block());
        self.key_is_branch(r, name, hit, miss);
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
        self.goto(head);
        self.switch_to(field_fail);
        self.json_prepend(ctx, Seg::Field(name));
        self.goto(fail);
        self.switch_to(miss);
    }

    /// An unknown key: skip its value (a malformed one fails at `.<key>`; with unknown keys
    /// rejected, any does).
    fn json_skip_member(&mut self, r: Local, ctx: Local, key: Local, head: BlockId, fail: BlockId) {
        let ok = self.rt_u8(Rt::JsonSkipUnknown, vec![Operand::Copy(Place::local(r))]);
        let (next, bad) = (self.new_block(), self.new_block());
        self.branch(ok, next, bad);
        self.switch_to(bad);
        // The key only for the path (the reader keeps it past its value).
        let kv = self.reader_call("key", vec![Operand::Copy(Place::local(r))]);
        self.assign(Place::local(key), Rvalue::Use(kv));
        let ka = self.addr(Place::local(key));
        self.json_prepend(ctx, Seg::Key(ka.clone()));
        self.call_rt(Rt::StrDrop, vec![ka], None);
        self.json_fail(ctx, "value", fail);
        self.switch_to(next);
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
