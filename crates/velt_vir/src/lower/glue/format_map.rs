//! Node's `Map(2) { 'a' => 1, 'b' => 2 }` (empty: `Map(0) {}`) for the prelude `Map` class
//! (std/prelude/map.vlt), instead of its private fields: the live entries of the dense
//! `entryKeys` / `entryValues` arrays in insertion order (a deleted entry's value is null).
//! A prelude `Record` (std/prelude/record.vlt, a `Map` in field 0) prints like the object it
//! stands for, in JavaScript's key order (array indices first: the entry positions its
//! `__positions` gives, json/map.rs): `{ a: 1, 'b c': 2 }` (empty: `{}`), and std's `Set` (std/collections/set.vlt, a
//! `Map<T, bool>` in field 0) like node's: `Set(2) { 1, 2 }` (empty: `Set(0) {}`). Past node's
//! depth limit they print as `[Map]`, `[Object]` and `[Set]`; a `Map` or `Set` shows its first
//! 100 entries, then `... n more items`, and stops reading there.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::format_array::MAX_ARRAY_LENGTH;
use crate::lower::glue::literals::inspect_key;
use crate::lower::json::RecordOrder;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Field indexes of the prelude `Map`.
const SIZE: u32 = 0;
const KEYS: u32 = 1;
const VALUES: u32 = 2;

/// How the entries of a prelude `Map` print.
#[derive(Clone, Copy, PartialEq)]
enum Entries {
    /// `k => v`, the first 100.
    Map,
    /// `k: v`, all of them (an object).
    Record,
    /// `k`, the first 100.
    Set,
}

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
        let named =
            a.name == "Record" || a.name.ends_with("::Record") || a.name.ends_with(".Record");
        if !named || a.fields.first().map(|f| f.name.as_str()) != Some("entries") {
            return None;
        }
        match args.as_slice() {
            [k, v] => Some((*k, *v)),
            _ => None,
        }
    }

    /// `(T, bool)` if `ty` is std's `Set<T>` class (a `Map<T, bool>` in field 0).
    pub(in crate::lower) fn std_set(&mut self, ty: TyId) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            return None;
        };
        let named = a.name == "Set" || a.name.ends_with("::Set") || a.name.ends_with(".Set");
        if !named || a.fields.first().map(|f| f.name.as_str()) != Some("items") {
            return None;
        }
        let items = self.cx.adt_field_tys(ty)[0];
        match self.prelude_map(items) {
            Some((t, v)) if matches!(self.cx.kind(v), TyKind::Bool) => Some((t, v)),
            _ => None,
        }
    }

    /// Append the prelude `Map`, `Record` or std `Set` object at `obj` (at node's depth
    /// `depth`) as node prints it; false if `ty` is none of them.
    pub(super) fn format_collection(
        &mut self,
        buf: &Operand,
        obj: &Place,
        ty: TyId,
        depth: &Operand,
    ) -> bool {
        // (the `Map` holding the entries, its type, key and value types, how they print)
        let (map, map_ty, kt, vt, entries) = if let Some((kt, vt)) = self.prelude_map(ty) {
            (obj.clone(), ty, kt, vt, Entries::Map)
        } else if let Some((kt, vt)) = self.prelude_record(ty) {
            let map_ty = self.cx.adt_field_tys(ty)[0];
            (
                self.field_place(obj, ty, 0),
                map_ty,
                kt,
                vt,
                Entries::Record,
            )
        } else if let Some((t, vt)) = self.std_set(ty) {
            let map_ty = self.cx.adt_field_tys(ty)[0];
            (self.field_place(obj, ty, 0), map_ty, t, vt, Entries::Set)
        } else {
            return false;
        };
        let (name, cut) = match entries {
            Entries::Map => (Some("Map"), "[Map]"),
            Entries::Record => (None, "[Object]"),
            Entries::Set => (Some("Set"), "[Set]"),
        };
        let mtys = self.cx.adt_field_tys(map_ty);
        let size_ty = mtys[SIZE as usize];
        let size = Operand::Copy(self.field_place(&map, map_ty, SIZE));
        let size_t = self.cx.ty(size_ty);
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, size.clone(), cint(0, size_t)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        self.push_text(buf, &name.map_or("{}".into(), |n| format!("{n}(0) {{}}")));
        self.goto(done);
        self.switch_to(full_bb);
        let p = Operand::Copy(obj.clone());
        self.within_depth(buf, depth, cut, Some(p.clone()), |lw, child| {
            lw.format_once(buf, p, |lw| {
                if let Some(n) = name {
                    lw.push_text(buf, &format!("{n}("));
                    lw.push_scalar(buf, size.clone(), size_ty);
                    lw.push_text(buf, ") ");
                }
                lw.push_text(buf, "{ ");
                let types = (map_ty, kt, vt);
                let order = (entries == Entries::Record).then(|| lw.record_order(obj, ty));
                let ord = order.as_ref();
                lw.format_map_entries(buf, &map, types, entries, size, &child, ord);
                if let Some(o) = order {
                    lw.drop_record_order(o);
                }
                lw.push_text(buf, " }");
            })
        });
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

    /// The live entries of the `Map` at `obj` (of types `(map, key, value)`) with `size` of
    /// them, comma-separated, each part at node's depth `child`; a `Map` or `Set` stops after
    /// the first 100 and adds `... n more items`.
    fn format_map_entries(
        &mut self,
        buf: &Operand,
        obj: &Place,
        (ty, kt, vt): (TyId, TyId, TyId),
        entries: Entries,
        size: Operand,
        child: &Operand,
        order: Option<&RecordOrder>,
    ) {
        let tys = self.cx.adt_field_tys(ty);
        let keys = self.field_place(obj, ty, KEYS);
        let keys = self.content(&keys, tys[KEYS as usize]);
        let values = self.field_place(obj, ty, VALUES);
        let values = self.content(&values, tys[VALUES as usize]);
        let TyKind::Array(slot_t) = self.cx.kind(tys[VALUES as usize]) else {
            crate::lower::ice("Map.entryValues is not an array")
        };
        let limited = entries != Entries::Record;
        // Entries shown so far; the loop ends early (`stop` set to 0) once the limit is reached.
        let shown = self.temp(Ty::U64);
        self.assign(Place::local(shown), Rvalue::Use(cint(0, Ty::U64)));
        let stop = self.temp(Ty::U64);
        let len = Operand::Copy(proj(&keys, Proj::Field(1)));
        let len = self.order_len(order, len);
        self.assign(Place::local(stop), Rvalue::Use(len));
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let n = Operand::Copy(Place::local(shown));
        self.count_loop(k, Operand::Copy(Place::local(stop)), |lw, i| {
            let i = lw.order_pos(order, i);
            let slot = lw.elem_place(&values, i.clone(), slot_t);
            let live = lw.option_is_some(&slot, slot_t);
            let (live_bb, skip) = (lw.new_block(), lw.new_block());
            lw.branch(live, live_bb, skip);
            lw.switch_to(live_bb);
            if limited {
                let limit = cint(MAX_ARRAY_LENGTH, Ty::U64);
                let full = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, n.clone(), limit));
                let (full_bb, room_bb) = (lw.new_block(), lw.new_block());
                lw.branch(full, full_bb, room_bb);
                lw.switch_to(full_bb);
                lw.assign(Place::local(stop), Rvalue::Use(cint(0, Ty::U64)));
                lw.goto(skip);
                lw.switch_to(room_bb);
            }
            let first = lw.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, n.clone(), cint(0, Ty::U64)),
            );
            let (sep_bb, entry_bb) = (lw.new_block(), lw.new_block());
            lw.branch(first, entry_bb, sep_bb);
            lw.switch_to(sep_bb);
            lw.push_text(buf, ", ");
            lw.goto(entry_bb);
            lw.switch_to(entry_bb);
            let next = lw.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, n, cint(1, Ty::U64)));
            lw.assign(Place::local(shown), Rvalue::Use(next));
            let kp = lw.elem_place(&keys, i, kt);
            match entries {
                Entries::Record => {
                    lw.format_record_key(buf, &kp, kt);
                    lw.push_text(buf, ": ");
                }
                Entries::Map | Entries::Set => lw.format_nested(buf, &kp, kt, child),
            }
            if entries != Entries::Set {
                if entries == Entries::Map {
                    lw.push_text(buf, " => ");
                }
                let vp = lw.some_payload(&slot, slot_t);
                lw.format_nested(buf, &vp, vt, child);
            }
            lw.goto(skip);
            lw.switch_to(skip);
        });
        if limited {
            self.format_more_entries(buf, size, tys[SIZE as usize]);
        }
    }

    /// Node's `... n more items` after the first 100 of `size` entries (of type `size_ty`).
    fn format_more_entries(&mut self, buf: &Operand, size: Operand, size_ty: TyId) {
        let from = self.cx.ty(size_ty);
        let size = self.cast_to(size, from, Ty::U64);
        let limit = cint(MAX_ARRAY_LENGTH, Ty::U64);
        let long = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Gt, size.clone(), limit.clone()),
        );
        let (more_bb, done) = (self.new_block(), self.new_block());
        self.branch(long, more_bb, done);
        self.switch_to(more_bb);
        let remaining = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, size, limit));
        self.call_rt(Rt::StrbufInspectMore, vec![buf.clone(), remaining], None);
        self.goto(done);
        self.switch_to(done);
    }
}
