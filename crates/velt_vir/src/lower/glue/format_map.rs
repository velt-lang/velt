//! Node's `Map(2) { 'a' => 1, 'b' => 2 }` (empty: `Map(0) {}`) for the prelude `Map` class
//! (std/prelude/map.vlt), instead of its private fields: the live entries of the dense
//! `entryKeys` / `entryValues` arrays in insertion order (a deleted entry's value is null).
//! A prelude `Record` (std/prelude/record.vlt, a `Map` in field 0) prints like the object it
//! stands for: `{ a: 1, 'b c': 2 }` (empty: `{}`).

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::glue::literals::inspect_key;
use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Field indexes of the prelude `Map`.
const SIZE: u32 = 0;
const KEYS: u32 = 1;
const VALUES: u32 = 2;

impl FnLower<'_, '_> {
    /// `(K, V)` if `ty` is the prelude `Map<K, V>` class.
    pub(in crate::lower) fn prelude_map(&mut self, ty: TyId) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            return None;
        };
        let named = a.name == "Map" || a.name.ends_with("::Map") || a.name.ends_with(".Map");
        let fields: Vec<&str> = a.fields.iter().map(|f| f.name.as_str()).collect();
        if !named || fields.get(..3) != Some(&["_size", "entryKeys", "entryValues"][..]) {
            return None;
        }
        let tys = self.cx.adt_field_tys(ty);
        match (self.cx.kind(tys[1]), self.cx.kind(tys[2])) {
            (TyKind::Array(k), TyKind::Array(slot)) => match self.cx.kind(slot) {
                TyKind::Option(v) => Some((k, v)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `(K, V)` if `ty` is the prelude `Record<K, V>` class (its `Map<K, V>` is field 0).
    pub(in crate::lower) fn prelude_record(&mut self, ty: TyId) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
            return None;
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            return None;
        };
        let named = a.name == "Record" || a.name.ends_with("::Record") || a.name.ends_with(".Record");
        if !named || a.fields.first().map(|f| f.name.as_str()) != Some("entries") {
            return None;
        }
        match args.as_slice() {
            [k, v] => Some((*k, *v)),
            _ => None,
        }
    }

    /// Append the prelude `Record` object at `obj` as an object; false if `ty` is not one.
    pub(super) fn format_record(&mut self, buf: &Operand, obj: &Place, ty: TyId) -> bool {
        let Some((kt, vt)) = self.prelude_record(ty) else {
            return false;
        };
        let map_ty = self.cx.adt_field_tys(ty)[0];
        let map = self.field_place(obj, ty, 0);
        let mtys = self.cx.adt_field_tys(map_ty);
        let size = self.field_place(&map, map_ty, SIZE);
        let size_t = self.cx.ty(mtys[SIZE as usize]);
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(size), cint(0, size_t)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        self.push_text(buf, "{}");
        self.goto(done);
        self.switch_to(full_bb);
        self.push_text(buf, "{ ");
        self.format_map_entries(buf, &map, map_ty, kt, vt, true);
        self.push_text(buf, " }");
        self.goto(done);
        self.switch_to(done);
        true
    }

    /// A record key: a string at run time, or the text of a literal / string enum member.
    fn format_record_key(&mut self, buf: &Operand, kp: &Place, kt: TyId) {
        if let TyKind::Str = self.cx.kind(kt) {
            let a = self.addr(kp.clone());
            self.call_rt(Rt::StrbufPushInspectKey, vec![buf.clone(), a], None);
            return;
        }
        // A union of literal types switches on its tag; a string enum on its member index.
        let (texts, value) = match self.enum_strings(kt) {
            Some(strings) => (strings, Operand::Copy(kp.clone())),
            None => {
                let n = match self.cx.kind(kt) {
                    TyKind::Adt(d, _) => self.cx.enum_def(d).variants.len() as u32,
                    _ => 0,
                };
                let texts = (0..n)
                    .map(|k| match self.variant_literal(kt, k) {
                        Some(LitValue::Str(s)) => s,
                        _ => String::new(),
                    })
                    .collect();
                (texts, Operand::Copy(proj(kp, Proj::Field(0))))
            }
        };
        let join = self.new_block();
        let blocks: Vec<vir::BlockId> = texts.iter().map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (i as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value,
            cases,
            default: join,
        });
        for (t, b) in texts.iter().zip(blocks) {
            self.switch_to(b);
            self.push_text(buf, &inspect_key(t));
            self.goto(join);
        }
        self.switch_to(join);
    }

    /// Append the prelude `Map` object at `obj` node-style; false if `ty` is not that `Map`.
    pub(super) fn format_map(&mut self, buf: &Operand, obj: &Place, ty: TyId) -> bool {
        let Some((kt, vt)) = self.prelude_map(ty) else {
            return false;
        };
        let tys = self.cx.adt_field_tys(ty);
        let size = self.field_place(obj, ty, SIZE);
        self.push_text(buf, "Map(");
        self.push_scalar(buf, Operand::Copy(size.clone()), tys[SIZE as usize]);
        let size_t = self.cx.ty(tys[SIZE as usize]);
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(size), cint(0, size_t)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        self.push_text(buf, ") {}");
        self.goto(done);
        self.switch_to(full_bb);
        self.push_text(buf, ") { ");
        self.format_map_entries(buf, obj, ty, kt, vt, false);
        self.push_text(buf, " }");
        self.goto(done);
        self.switch_to(done);
        true
    }

    /// `k => v` (`record`: `k: v`) for every live entry, comma-separated.
    fn format_map_entries(
        &mut self,
        buf: &Operand,
        obj: &Place,
        ty: TyId,
        kt: TyId,
        vt: TyId,
        record: bool,
    ) {
        let tys = self.cx.adt_field_tys(ty);
        let keys = self.field_place(obj, ty, KEYS);
        let values = self.field_place(obj, ty, VALUES);
        let TyKind::Array(slot_t) = self.cx.kind(tys[VALUES as usize]) else {
            crate::lower::ice("Map.entryValues is not an array")
        };
        let first = self.temp(Ty::Bool);
        let yes = Operand::Const(Const::Bool(true), Ty::Bool);
        self.assign(Place::local(first), Rvalue::Use(yes));
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(proj(&keys, Proj::Field(1)));
        self.count_loop(k, len, |lw, i| {
            let slot = lw.elem_place(&values, i.clone(), slot_t);
            let live = lw.option_is_some(&slot, slot_t);
            let (live_bb, skip) = (lw.new_block(), lw.new_block());
            lw.branch(live, live_bb, skip);
            lw.switch_to(live_bb);
            let (sep_bb, entry_bb) = (lw.new_block(), lw.new_block());
            lw.branch(Operand::Copy(Place::local(first)), entry_bb, sep_bb);
            lw.switch_to(sep_bb);
            lw.push_text(buf, ", ");
            lw.goto(entry_bb);
            lw.switch_to(entry_bb);
            let no = Operand::Const(Const::Bool(false), Ty::Bool);
            lw.assign(Place::local(first), Rvalue::Use(no));
            let kp = lw.elem_place(&keys, i, kt);
            if record {
                lw.format_record_key(buf, &kp, kt);
                lw.push_text(buf, ": ");
            } else {
                lw.format_nested(buf, &kp, kt);
                lw.push_text(buf, " => ");
            }
            let vp = lw.some_payload(&slot, slot_t);
            lw.format_nested(buf, &vp, vt);
            lw.goto(skip);
            lw.switch_to(skip);
        });
    }
}
