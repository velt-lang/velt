//! The key `JSON.stringify` passes to a `toJSON(key)`, as JavaScript does: the property name
//! for a member, the index (as a string) for an array element, `""` for the top value.
//!
//! Only a type that may reach such a method needs its key ([`Cx::json_needs_key`]): a class
//! whose `toJSON` takes the key (through sema's adapter), one with such a JSON-writable
//! descendant, or a `T | null`, shared value or union holding one. Such a type is written
//! through `Glue::JsonWriteKey`, which takes the key; everything else keeps `Glue::JsonWrite`,
//! so a program without a `toJSON(key)` writes exactly as before.

use std::collections::HashMap;

use velt_sema::hir::{self, AdtKind, TyId, TyKind};

use crate::lower::rt::Rt;
use crate::lower::{cint, Cx, FnLower, Glue};
use crate::vir::{Operand, Place, Ty, STR_AGG};

/// Where a value's key comes from.
pub(super) enum JsonKey<'a> {
    /// A known name (a field, a tuple position, `""` for the top value).
    Text(&'a str),
    /// An array index (a `u64`), written as its decimal string.
    Index(Operand),
    /// A string at a place (a `Map` key).
    Str(&'a Place),
}

impl Cx<'_> {
    /// Is some class's `toJSON` hook given the key?
    fn any_keyed_to_json(&mut self) -> bool {
        (0..self.hir.defs.len()).any(|i| match self.hir.def(hir::DefId(i as u32)) {
            hir::Def::Adt(a) => a.to_json.is_some_and(|m| self.fn_def(m).params.len() == 2),
            _ => false,
        })
    }

    /// Does writing a `ty` call a `toJSON(key)`, so that its writer needs the key?
    pub(super) fn json_needs_key(&mut self, ty: TyId) -> bool {
        if self.json_key_memo.is_none() {
            let keyed = self.any_keyed_to_json();
            self.json_key_memo = Some((keyed, HashMap::new()));
        }
        let Some((keyed, memo)) = &mut self.json_key_memo else {
            return false;
        };
        if !*keyed {
            return false;
        }
        if let Some(&b) = memo.get(&ty) {
            return b;
        }
        // A cycle through unions or options is not keyed by itself.
        memo.insert(ty, false);
        let b = self.json_needs_key_uncached(ty);
        if let Some((_, memo)) = &mut self.json_key_memo {
            memo.insert(ty, b);
        }
        b
    }

    fn json_needs_key_uncached(&mut self, ty: TyId) -> bool {
        match self.kind(ty) {
            TyKind::Option(e) | TyKind::Shared(e) => self.json_needs_key(e),
            TyKind::Adt(d, args) => match self.hir.def(d) {
                hir::Def::Adt(a) if a.kind == AdtKind::Class => {
                    if self.keyed_class(ty) {
                        return true;
                    }
                    let subs = match self.has_header(d) {
                        true => self.json_descendants(ty),
                        false => vec![],
                    };
                    subs.into_iter().any(|s| self.keyed_class(s))
                }
                hir::Def::Enum(_) if self.is_union(ty) => {
                    let payloads: Vec<TyId> = self
                        .enum_def(d)
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.clone())
                        .collect();
                    payloads.into_iter().any(|p| {
                        let p = self.subst(p, &args);
                        self.json_needs_key(p)
                    })
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Does class type `ty` have a `toJSON` hook that takes the key?
    fn keyed_class(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.kind(ty) else {
            return false;
        };
        match self.hir.def(d) {
            hir::Def::Adt(a) => a.to_json.is_some_and(|m| self.fn_def(m).params.len() == 2),
            _ => false,
        }
    }
}

impl FnLower<'_, '_> {
    /// Append `*place` (of concrete type `ty`), whose key in its parent is `key`, to the builder
    /// at `buf`.
    pub(super) fn json_write_at(&mut self, buf: &Operand, place: &Place, ty: TyId, key: JsonKey) {
        if !self.cx.json_needs_key(ty) {
            return self.json_write(buf, place, ty);
        }
        let (kp, owned) = self.json_key_place(key);
        self.json_write_with(buf, place, ty, Some(&kp));
        if owned {
            let s = self.cx.str_ty();
            self.drop_glue(kp, s);
        }
    }

    /// Append `*place` (of concrete type `ty`) with the key string at `key`, if it has one.
    pub(super) fn json_write_with(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        key: Option<&Place>,
    ) {
        match key {
            Some(k) if self.cx.json_needs_key(ty) => {
                let a = self.addr(place.clone());
                let ka = self.addr(k.clone());
                self.call_glue(Glue::JsonWriteKey, ty, vec![buf.clone(), a, ka]);
            }
            _ => self.json_write(buf, place, ty),
        }
    }

    /// The place of `key` as a string, and whether it is a new string to drop after use.
    fn json_key_place(&mut self, key: JsonKey) -> (Place, bool) {
        let st = Ty::Agg(STR_AGG);
        match key {
            JsonKey::Text(s) => {
                let v = self.str_lit(s);
                let t = self.copy_to_temp(v, st);
                (Place::local(t), false)
            }
            JsonKey::Index(i) => {
                let b = self.temp(st);
                let bp = self.addr(Place::local(b));
                self.call_rt(Rt::StrbufNew, vec![cint(0, Ty::U64), bp.clone()], None);
                self.call_rt(Rt::StrbufPushU64, vec![bp.clone(), i], None);
                let out = self.temp(st);
                let op = self.addr(Place::local(out));
                self.call_rt(Rt::StrbufFinish, vec![bp, op], None);
                (Place::local(out), true)
            }
            JsonKey::Str(p) => (p.clone(), false),
        }
    }
}
