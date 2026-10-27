//! Node's `Map(2) { 'a' => 1, 'b' => 2 }` (empty: `Map(0) {}`) for the prelude `Map` class
//! (std/prelude/map.vlt), instead of its private fields: the live entries of the dense
//! `entryKeys` / `entryValues` arrays in insertion order (a deleted entry's value is null; where
//! the value type includes null, `map_entry_live` asks the map's `__liveAt`).
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
    /// `Symbol(d): v`, all of them (a record's symbol table, after its string keys).
    Symbols,
}

impl FnLower<'_, '_> {
    /// `(K, V)` if `ty` is the prelude `Map<K, V>` class.
    pub(in crate::lower) fn prelude_map(&mut self, ty: TyId) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
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
            // `V` from the type arguments: where it includes null, the slot type `V | null` is
            // `V` itself, not an option of it.
            (TyKind::Array(k), TyKind::Array(slot)) => match (self.cx.kind(slot), &args[..]) {
                (TyKind::Option(..), [_, v]) => Some((k, *v)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether entry position `i` of the prelude `Map` at `map` (of type `ty`, values of type
    /// `vt`) is live, with the place of its value: `slot` is the entry's `entryValues` element
    /// (of type `slot_t`). A null slot is a deleted entry unless `vt` includes null (the slot
    /// type is then `vt` itself and the slot the value); then the map's `__liveAt` decides
    /// (std/prelude/map.vlt).
    pub(in crate::lower) fn map_entry_live(
        &mut self,
        map: &Place,
        ty: TyId,
        (i, slot, slot_t, vt): (Operand, &Place, TyId, TyId),
    ) -> (Operand, Place) {
        if slot_t != vt {
            let live = self.option_is_some(slot, slot_t);
            return (live, self.some_payload(slot, slot_t));
        }
        let TyKind::Adt(d, targs) = self.cx.kind(ty) else {
            crate::lower::ice("Map of a non-ADT type")
        };
        let def = self.class_method(d, "__liveAt");
        let f = self.cx.func_for_direct(def, targs, self.mode);
        let live = self.temp(Ty::Bool);
        let args = vec![Operand::Copy(map.clone()), i];
        self.call(vir::Callee::Func(f), args, Some(Place::local(live)), false);
        (Operand::Copy(Place::local(live)), slot.clone())
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
        // A subclass of `Map` or `Set`: its base's entries, then its own fields, under its own
        // name (`M(1) [Map] { 'a' => 1, tag: 't' }`, `[M [Map]]` past the depth limit), as
        // Node writes it.
        if let Some((base, sub)) = self.collection_subclass(ty) {
            let (kind, set) = match self.prelude_map(base) {
                Some(_) => ("Map", false),
                None => ("Set", true),
            };
            let (kt, vt) = match set {
                false => self.prelude_map(base),
                true => self.std_set(base),
            }
            .unwrap_or_else(|| crate::lower::ice("a collection base"));
            let (map, map_ty, entries) = match set {
                false => (obj.clone(), base, Entries::Map),
                true => (
                    self.field_place(obj, ty, 0),
                    self.cx.adt_field_tys(base)[0],
                    Entries::Set,
                ),
            };
            let name = format!("{sub}(");
            let tag = format!(") [{kind}] ");
            let cut = format!("[{sub} [{kind}]]");
            let parts = (map, map_ty, kt, vt, entries);
            let TyKind::Adt(b, _) = self.cx.kind(base) else {
                crate::lower::ice("a collection base")
            };
            let inherited = self.cx.adt_def(b).fields.len() as u32;
            let mut own = self.shown_fields(ty);
            own.retain(|(i, _, _)| *i >= inherited);
            let texts = (name.as_str(), tag.as_str(), cut.as_str());
            self.format_entries_named(buf, obj, ty, depth, parts, &own, texts);
            return true;
        }
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
            Entries::Record | Entries::Symbols => (None, "[Object]"),
            Entries::Set => (Some("Set"), "[Set]"),
        };
        let parts = (map, map_ty, kt, vt, entries);
        if entries == Entries::Record && self.cx.hir.null_proto_records {
            // A record made by `Object.create(null)` (its map is marked): Node's
            // `[Object: null prototype] { ... }`, and `[Object: null prototype]` past the depth
            // limit.
            let np = self.null_proto_mark(&parts.0, map_ty);
            let (np_bb, plain_bb, done) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(np, np_bb, plain_bb);
            self.switch_to(np_bb);
            let prefix = "[Object: null prototype] ";
            let np_cut = "[Object: null prototype]";
            self.format_entries(buf, obj, ty, depth, parts.clone(), (name, prefix, np_cut));
            self.goto(done);
            self.switch_to(plain_bb);
            self.format_entries(buf, obj, ty, depth, parts, (name, "", cut));
            self.goto(done);
            self.switch_to(done);
            return true;
        }
        self.format_entries(buf, obj, ty, depth, parts, (name, "", cut));
        true
    }

    /// `(base, name)` if `ty` is a user class extending the prelude `Map` or std's `Set`
    /// (through other classes too): the base's type, with its arguments, and the class's short
    /// name.
    pub(in crate::lower) fn collection_subclass(&mut self, ty: TyId) -> Option<(TyId, String)> {
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
            return None;
        };
        let hir::Def::Adt(a) = self.cx.hir.def(d) else {
            return None;
        };
        if a.name.starts_with("std/") {
            return None;
        }
        let mut base = self.cx.subst(a.base?, &args);
        for _ in 0..16 {
            if self.prelude_map(base).is_some() || self.std_set(base).is_some() {
                return Some((base, self.cx.type_name(ty)));
            }
            let TyKind::Adt(b, bargs) = self.cx.kind(base) else {
                return None;
            };
            let next = self.cx.adt_def(b).base?;
            base = self.cx.subst(next, &bargs);
        }
        None
    }

    /// [`Self::format_entries`] under a subclass's name, then its `own` fields:
    /// `{name}{size}{tag}{ … }`, `cut` past node's depth limit.
    #[allow(clippy::too_many_arguments)] // `format_entries`' arguments, plus the fields
    fn format_entries_named(
        &mut self,
        buf: &Operand,
        obj: &Place,
        ty: TyId,
        depth: &Operand,
        parts: (Place, TyId, TyId, TyId, Entries),
        own: &[(u32, String, TyId)],
        (name, tag, cut): (&str, &str, &str),
    ) {
        let (map, map_ty, kt, vt, entries) = parts;
        let mtys = self.cx.adt_field_tys(map_ty);
        let size_ty = mtys[SIZE as usize];
        let size = Operand::Copy(self.field_place(&map, map_ty, SIZE));
        let size_t = self.cx.ty(size_ty);
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, size.clone(), cint(0, size_t)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        if own.is_empty() {
            self.branch(empty.clone(), empty_bb, full_bb);
            self.switch_to(empty_bb);
            self.push_text(buf, &format!("{name}0{tag}{{}}"));
            self.goto(done);
        } else {
            self.goto(full_bb);
        }
        self.switch_to(full_bb);
        let p = Operand::Copy(obj.clone());
        self.within_depth(buf, depth, cut, Some(p.clone()), |lw, child| {
            lw.format_once(buf, p, |lw| {
                lw.push_text(buf, name);
                lw.push_scalar(buf, size.clone(), size_ty);
                lw.push_text(buf, tag);
                lw.push_text(buf, "{ ");
                let types = (map_ty, kt, vt);
                if own.is_empty() {
                    lw.format_map_entries(buf, &map, types, (entries, None, None), size, &child);
                } else {
                    // The entries, if any, then each field after a comma.
                    let (some, none) = (lw.new_block(), lw.new_block());
                    lw.branch(empty, none, some);
                    lw.switch_to(some);
                    lw.format_map_entries(buf, &map, types, (entries, None, None), size, &child);
                    lw.push_text(buf, ", ");
                    lw.goto(none);
                    lw.switch_to(none);
                    for (i, (index, n, t)) in own.iter().enumerate() {
                        let sep = if i > 0 { ", " } else { "" };
                        lw.push_text(buf, &format!("{sep}{n}: "));
                        let fp = lw.field_place(obj, ty, *index);
                        lw.format_nested(buf, &fp, *t, &child);
                    }
                }
                lw.push_text(buf, " }");
            })
        });
        self.goto(done);
        self.switch_to(done);
    }

    /// Is the prelude `Map` at `map` (of type `map_ty`) marked as a prototype-less record's
    /// entries (the top bit of its `edits`, std/prelude/map.vlt `__markNullProto`)? A Bool
    /// operand.
    pub(in crate::lower) fn null_proto_mark(&mut self, map: &Place, map_ty: TyId) -> Operand {
        let TyKind::Adt(d, _) = self.cx.kind(map_ty) else {
            crate::lower::ice("the prelude `Map` is a class");
        };
        let at = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .position(|f| f.name == "edits");
        let at = at.unwrap_or_else(|| crate::lower::ice("the prelude `Map` has `edits`")) as u32;
        let edits = Operand::Copy(self.field_place(map, map_ty, at));
        let bit = cint(0x8000_0000_0000_0000_i128, Ty::U64);
        let marked = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitAnd, edits, bit));
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, marked, cint(0, Ty::U64)),
        )
    }

    /// The entries of a collection (`parts`: its `Map`, the map's type, key and value types,
    /// how they print) in braces, after `prefix` and the collection's `name`; `cut` past node's
    /// depth limit.
    fn format_entries(
        &mut self,
        buf: &Operand,
        obj: &Place,
        ty: TyId,
        depth: &Operand,
        parts: (Place, TyId, TyId, TyId, Entries),
        (name, prefix, cut): (Option<&str>, &str, &str),
    ) {
        let (map, map_ty, kt, vt, entries) = parts;
        let mtys = self.cx.adt_field_tys(map_ty);
        let size_ty = mtys[SIZE as usize];
        let size = Operand::Copy(self.field_place(&map, map_ty, SIZE));
        let size_t = self.cx.ty(size_ty);
        let mut empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, size.clone(), cint(0, size_t)),
        );
        // A record's symbol keys (field 2, `null` until the first is written).
        let symbols = (entries == Entries::Record)
            .then(|| self.record_symbols(obj, ty))
            .flatten();
        if let Some((_, _, n)) = &symbols {
            let none = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, n.clone(), cint(0, Ty::U64)),
            );
            empty = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, empty, none));
        }
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        let shown = name.map_or("{}".into(), |n| format!("{n}(0) {{}}"));
        self.push_text(buf, &format!("{prefix}{shown}"));
        self.goto(done);
        self.switch_to(full_bb);
        let p = Operand::Copy(obj.clone());
        self.within_depth(buf, depth, cut, Some(p.clone()), |lw, child| {
            lw.format_once(buf, p, |lw| {
                if !prefix.is_empty() {
                    lw.push_text(buf, prefix);
                }
                if let Some(n) = name {
                    lw.push_text(buf, &format!("{n}("));
                    lw.push_scalar(buf, size.clone(), size_ty);
                    lw.push_text(buf, ") ");
                }
                lw.push_text(buf, "{ ");
                let types = (map_ty, kt, vt);
                let order = (entries == Entries::Record).then(|| lw.record_order(obj, ty));
                let ord = order.as_ref();
                let shown =
                    lw.format_map_entries(buf, &map, types, (entries, ord, None), size, &child);
                if let Some(o) = order {
                    lw.drop_record_order(o);
                }
                if let Some((sym_map, sym_ty, n)) = symbols {
                    let (some, after) = (lw.new_block(), lw.new_block());
                    let any = lw.rvalue_temp(
                        Ty::Bool,
                        Rvalue::Binary(BinOp::Ne, n.clone(), cint(0, Ty::U64)),
                    );
                    lw.branch(any, some, after);
                    lw.switch_to(some);
                    let symbol = lw.cx.intern(TyKind::Symbol);
                    let types = (sym_ty, symbol, vt);
                    let start = Some(Operand::Copy(Place::local(shown)));
                    let how = (Entries::Symbols, None, start);
                    lw.format_map_entries(buf, &sym_map, types, how, n, &child);
                    lw.goto(after);
                    lw.switch_to(after);
                }
                lw.push_text(buf, " }");
            })
        });
        self.goto(done);
        self.switch_to(done);
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
    /// the first 100 and adds `... n more items`. A record's entries come in `order` (its
    /// `__positions`).
    fn format_map_entries(
        &mut self,
        buf: &Operand,
        obj: &Place,
        (ty, kt, vt): (TyId, TyId, TyId),
        (entries, order, start): (Entries, Option<&RecordOrder>, Option<Operand>),
        size: Operand,
        child: &Operand,
    ) -> vir::Local {
        let tys = self.cx.adt_field_tys(ty);
        let keys = self.field_place(obj, ty, KEYS);
        let keys = self.content(&keys, tys[KEYS as usize]);
        let values = self.field_place(obj, ty, VALUES);
        let values = self.content(&values, tys[VALUES as usize]);
        let TyKind::Array(slot_t) = self.cx.kind(tys[VALUES as usize]) else {
            crate::lower::ice("Map.entryValues is not an array")
        };
        let limited = !matches!(entries, Entries::Record | Entries::Symbols);
        // Entries shown so far (`start`: those shown before, a record's string keys before its
        // symbols); the loop ends early (`stop` set to 0) once the limit is reached.
        let shown = self.temp(Ty::U64);
        let first = start.unwrap_or(cint(0, Ty::U64));
        self.assign(Place::local(shown), Rvalue::Use(first));
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
            let (live, vp) = lw.map_entry_live(obj, ty, (i.clone(), &slot, slot_t, vt));
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
                Entries::Symbols => {
                    // Node 26 writes a symbol key as `Symbol(d): v`.
                    lw.format_nested(buf, &kp, kt, child);
                    lw.push_text(buf, ": ");
                }
                Entries::Map | Entries::Set => lw.format_nested(buf, &kp, kt, child),
            }
            if entries != Entries::Set {
                if entries == Entries::Map {
                    lw.push_text(buf, " => ");
                }
                lw.format_nested(buf, &vp, vt, child);
            }
            lw.goto(skip);
            lw.switch_to(skip);
        });
        if limited {
            self.format_more_entries(buf, size, tys[SIZE as usize]);
        }
        shown
    }

    /// The symbol table of the prelude `Record` at `obj` (field 2, a `Map<symbol, V> | null`):
    /// the map's place and type, and its size (0 when there is none) as a `u64`. `None` for a
    /// record without the field.
    fn record_symbols(&mut self, obj: &Place, ty: TyId) -> Option<(Place, TyId, Operand)> {
        let tys = self.cx.adt_field_tys(ty);
        let opt = *tys.get(2)?;
        let TyKind::Option(map_ty, _) = self.cx.kind(opt) else {
            return None;
        };
        self.prelude_map(map_ty)?;
        let fp = self.field_place(obj, ty, 2);
        let some = self.option_is_some(&fp, opt);
        let n = self.temp(Ty::U64);
        self.assign(Place::local(n), Rvalue::Use(cint(0, Ty::U64)));
        let (yes, done) = (self.new_block(), self.new_block());
        self.branch(some, yes, done);
        self.switch_to(yes);
        let map = self.some_payload(&fp, opt);
        let mtys = self.cx.adt_field_tys(map_ty);
        let size_ty = mtys[SIZE as usize];
        let size = Operand::Copy(self.field_place(&map, map_ty, SIZE));
        let from = self.cx.ty(size_ty);
        let size = self.cast_to(size, from, Ty::U64);
        self.assign(Place::local(n), Rvalue::Use(size));
        self.goto(done);
        self.switch_to(done);
        Some((map, map_ty, Operand::Copy(Place::local(n))))
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
