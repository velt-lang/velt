//! `Map<string, V>` as a JSON object: written from its live entries in insertion order, read
//! by `new Map()` and `set(key, value)` per member (a repeated key replaces the value, like
//! `JSON.parse`). Sema only lets maps with `string` keys through.

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::{Seg, STR};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower, ScopeKind};
use crate::vir::{self, BlockId, Const, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Field indexes of the prelude `Map` (see glue/format_map.rs).
const KEYS: u32 = 1;
const VALUES: u32 = 2;

impl FnLower<'_, '_> {
    /// `{"k":v,...}` for the prelude `Map` object at `place` (string keys).
    pub(super) fn json_write_map(&mut self, buf: &Operand, place: &Place, ty: TyId, vt: TyId) {
        let tys = self.cx.adt_field_tys(ty);
        let keys = self.field_place(place, ty, KEYS);
        let values = self.field_place(place, ty, VALUES);
        let TyKind::Array(slot_t) = self.cx.kind(tys[VALUES as usize]) else {
            ice("Map.entryValues is not an array")
        };
        self.push_text(buf, "{");
        let first = self.temp(Ty::Bool);
        let yes = Operand::Const(Const::Bool(true), Ty::Bool);
        self.assign(Place::local(first), Rvalue::Use(yes));
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(proj(&keys, Proj::Field(1)));
        let str_ty = self.cx.intern(TyKind::Str);
        self.count_loop(k, len, |lw, i| {
            // A deleted entry's value slot is null.
            let slot = lw.elem_place(&values, i.clone(), slot_t);
            let live = lw.option_is_some(&slot, slot_t);
            let (live_bb, skip) = (lw.new_block(), lw.new_block());
            lw.branch(live, live_bb, skip);
            lw.switch_to(live_bb);
            let (sep_bb, entry_bb) = (lw.new_block(), lw.new_block());
            lw.branch(Operand::Copy(Place::local(first)), entry_bb, sep_bb);
            lw.switch_to(sep_bb);
            lw.push_text(buf, ",");
            lw.goto(entry_bb);
            lw.switch_to(entry_bb);
            let no = Operand::Const(Const::Bool(false), Ty::Bool);
            lw.assign(Place::local(first), Rvalue::Use(no));
            let kp = lw.elem_place(&keys, i, str_ty);
            let ka = lw.addr(kp);
            lw.call_rt(Rt::StrbufPushJsonStr, vec![buf.clone(), ka], None);
            lw.push_text(buf, ":");
            let vp = lw.some_payload(&slot, slot_t);
            lw.json_write(buf, &vp, vt);
            lw.goto(skip);
            lw.switch_to(skip);
        });
        self.push_text(buf, "}");
    }

    /// Decode an object into a new prelude `Map<string, V>` at `place`.
    pub(super) fn json_read_map(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        ty: TyId,
        vt: TyId,
        fail: BlockId,
    ) {
        let TyKind::Adt(map, targs) = self.cx.kind(ty) else {
            ice("JSON map of a non-ADT type")
        };
        // `new Map()` runs the field initializers, which may create statement temporaries.
        self.push_scope(ScopeKind::Temps);
        let obj = self.new_object(ty, &[]);
        if let Operand::Copy(p) = &obj {
            self.take_temp(p);
        }
        self.assign(place.clone(), Rvalue::Use(obj));
        self.pop_scope();
        let set = self.class_method(map, "set");
        let ro = Operand::Copy(Place::local(r));
        self.json_expect(Rt::JsonObjectStart, vec![ro.clone()], ctx, "object", fail);
        let key = self.temp(STR);
        let (head, body, done, bad) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.goto(head);
        self.switch_to(head);
        let step = self.temp(Ty::U8);
        let ka = self.addr(Place::local(key));
        self.call_rt(Rt::JsonNextKey, vec![ro, ka.clone()], Some(Place::local(step)));
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(step)),
            cases: vec![(1, body), (0, done)],
            default: bad,
        });
        self.switch_to(bad);
        self.json_fail(ctx, "object", fail);

        // The key borrows the source: keep an owned copy for the map.
        self.switch_to(body);
        let owned = self.temp(STR);
        let oa = self.addr(Place::local(owned));
        self.call_rt(Rt::StrClone, vec![ka.clone(), oa.clone()], None);
        self.call_rt(Rt::StrDrop, vec![ka], None);
        let vty = self.cx.ty(vt);
        let value = (vty != Ty::Unit).then(|| self.temp(vty));
        let dummy = self.temp(Ty::U8);
        let vplace = Place::local(value.unwrap_or(dummy));
        if let Some(v) = value {
            self.json_init(v, vt);
        }
        let value_fail = self.new_block();
        self.json_read(r, &vplace, ctx, vt, value_fail);
        let this = Operand::Copy(place.clone());
        self.json_call_set(set, targs, this, owned, value, vt);
        self.goto(head);
        self.switch_to(value_fail);
        self.json_prepend(ctx, Seg::Key(oa.clone()));
        self.call_rt(Rt::StrDrop, vec![oa], None);
        self.goto(fail);
        self.switch_to(done);
    }

    /// `this.set(key, value)`: the owned `key` and `value` temps are moved into the call, or
    /// dropped after it when the callee only borrows them.
    fn json_call_set(
        &mut self,
        set: DefId,
        targs: Vec<TyId>,
        this: Operand,
        key: Local,
        value: Option<Local>,
        vt: TyId,
    ) {
        let f = self.cx.fn_def(set);
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        let (ret, throws) = self.cx.call_sig(f);
        let str_ty = self.cx.intern(TyKind::Str);
        let mut argv = vec![this];
        let mut borrowed = vec![];
        let args = [(Some(key), str_ty), (value, vt)];
        for ((l, t), mode) in args.into_iter().zip(&modes[1..]) {
            let Some(l) = l else { continue };
            match (self.cx.ty(t), mode) {
                (Ty::Agg(_), PassMode::Owned) => argv.push(self.addr(Place::local(l))),
                (Ty::Agg(_), _) => {
                    argv.push(self.addr(Place::local(l)));
                    borrowed.push((l, t));
                }
                (_, PassMode::Owned | PassMode::Copy) => argv.push(Operand::Copy(Place::local(l))),
                _ => {
                    argv.push(Operand::Copy(Place::local(l)));
                    borrowed.push((l, t));
                }
            }
        }
        let fid = self.cx.func_for(set, targs.clone());
        let ret = self.cx.subst(ret, &targs);
        let throws = throws.map(|e| self.cx.subst(e, &targs));
        let throws = self.cx.error_ty(throws);
        self.finish_call(vir::Callee::Func(fid), argv, ret, throws);
        for (l, t) in borrowed {
            self.drop_glue(Place::local(l), t);
        }
    }

    /// The method `name` of class `class` (a prelude class whose methods the glue calls).
    fn class_method(&mut self, class: DefId, name: &str) -> DefId {
        let owner = self.cx.adt_def(class).name.clone();
        let full = format!("{owner}.{name}");
        let found = self.cx.hir.defs.iter().position(|d| match d {
            hir::Def::Fn(f) => f.name == full && f.self_ty.is_some(),
            _ => false,
        });
        let i = found.unwrap_or_else(|| ice(format_args!("no method `{full}`")));
        DefId(i as u32)
    }
}
