//! `JSON.stringify` of a class value serializes the object's *dynamic* class (JS writes an
//! object's own fields): a `Base`-typed value holding a `Derived` writes `Derived`'s fields.
//!
//! The writer of a class with subclasses compares the object's vtable pointer with the vtable
//! of each concrete descendant and calls that descendant's writer on a match; otherwise it
//! writes the static class. This is a type switch rather than a vtable slot so that JSON glue
//! is only generated for hierarchies that are actually stringified. Descendants are included
//! when their type args follow from the static class's (`class D<T> extends B<T>`) and all of
//! their fields have a JSON form (sema only checked the static type); any other descendant is
//! written as the static class, as before.

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use crate::lower::{Cx, FnLower, Glue, VtableKey};
use crate::vir::{BinOp, Operand, Place, Rvalue, Ty};

impl Cx<'_> {
    /// Concrete strict descendants of class type `cls` that can be written as JSON.
    fn json_descendants(&mut self, cls: TyId) -> Vec<TyId> {
        let TyKind::Adt(target, args) = self.kind(cls) else {
            return vec![];
        };
        let mut out = vec![];
        for i in 0..self.hir.defs.len() {
            let d = DefId(i as u32);
            if d == target {
                continue;
            }
            let hir::Def::Adt(a) = self.hir.def(d) else {
                continue;
            };
            if a.kind != AdtKind::Class || a.base.is_none() {
                continue;
            }
            let Some(sub) = self.instantiate_descendant(d, target, &args) else {
                continue;
            };
            if self.json_writable(sub, &mut vec![]) {
                out.push(sub);
            }
        }
        out
    }

    /// `d<…>` whose ancestor of def `target` is `target<args>`, if `d` descends from `target`
    /// and every type parameter of `d` is determined by that ancestor.
    fn instantiate_descendant(&mut self, d: DefId, target: DefId, args: &[TyId]) -> Option<TyId> {
        let n = self.adt_def(d).generics as usize;
        // `d`'s ancestors as patterns over `d`'s own type parameters.
        let own: Vec<TyId> = (0..n as u32)
            .map(|p| self.intern(TyKind::Param(p)))
            .collect();
        let mut cur = self.intern(TyKind::Adt(d, own));
        let mut guard = 0;
        loop {
            let TyKind::Adt(cd, cargs) = self.kind(cur) else {
                return None;
            };
            if cd == target {
                let mut slots = vec![None; n];
                let pattern = self.intern(TyKind::Adt(cd, cargs));
                let concrete = self.intern(TyKind::Adt(target, args.to_vec()));
                if !self.match_ty(pattern, concrete, &mut slots) || slots.len() != n {
                    return None;
                }
                let slots: Option<Vec<TyId>> = slots.into_iter().collect();
                return Some(self.intern(TyKind::Adt(d, slots?)));
            }
            guard += 1;
            let base = self.adt_def(cd).base?;
            if guard > 64 {
                return None;
            }
            cur = self.subst(base, &cargs);
        }
    }

    /// Does `t` have a JSON form? The lowering-side mirror of sema's stringify check
    /// (`velt_sema::json`): numbers, bool, string, literals, arrays, tuples, `T | null`, C-like
    /// enums, unions of writable members, `json.Value`, `Map<string, V>`, and structs, classes
    /// and object literals without private fields whose fields are writable. `stack` holds the
    /// ADTs being visited (recursive types).
    fn json_writable(&mut self, t: TyId, stack: &mut Vec<TyId>) -> bool {
        match self.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => true,
            TyKind::Literal(_) => true,
            TyKind::Array(e) | TyKind::Option(e) => self.json_writable(e, stack),
            TyKind::Tuple(es) => es.into_iter().all(|e| self.json_writable(e, stack)),
            TyKind::Adt(d, args) => {
                if stack.contains(&t) || self.is_json_value(t) {
                    return true;
                }
                let Some(members) = self.json_members(d, &args) else {
                    return false;
                };
                stack.push(t);
                let ok = members.into_iter().all(|m| self.json_writable(m, stack));
                stack.pop();
                ok
            }
            _ => false,
        }
    }

    /// The parts of ADT `d<args>` that must be writable: fields, union members' payloads, or
    /// the value type of a `Map<string, V>`, or nothing for C-like enums. `None`: no JSON form
    /// (other maps, payload enums, types holding std's private state).
    fn json_members(&mut self, d: DefId, args: &[TyId]) -> Option<Vec<TyId>> {
        let tys: Vec<TyId> = match self.hir.def(d) {
            // `Record<K, V>` is an object (sema checked its keys).
            hir::Def::Adt(a) if is_record(&a.name) => match args {
                [_, v] => return Some(vec![*v]),
                _ => return None,
            },
            // `Map<string, V>` is an object.
            hir::Def::Adt(a) if is_map(&a.name) => match args {
                [k, v] if matches!(self.kind(*k), TyKind::Str) => return Some(vec![*v]),
                _ => return None,
            },
            // std's private state (runtime handles) has no JSON form.
            hir::Def::Adt(a) if a.opaque => return None,
            // ES private fields (`#x`) are never written; `private x` is, as in Node.
            hir::Def::Adt(a) => a
                .fields
                .iter()
                .filter(|f| !f.name.starts_with('#'))
                .map(|f| f.ty)
                .collect(),
            hir::Def::Enum(e) if e.variants.iter().all(|v| v.payload.is_empty()) => vec![],
            hir::Def::Enum(e) if e.is_union => e
                .variants
                .iter()
                .flat_map(|v| v.payload.iter().copied())
                .collect(),
            _ => return None,
        };
        Some(tys.into_iter().map(|f| self.subst(f, args)).collect())
    }
}

/// The prelude's `Record`.
fn is_record(name: &str) -> bool {
    name == "Record" || name.ends_with("::Record") || name.ends_with(".Record")
}

/// The prelude's `Map` (serialized by sema's rules as having no JSON form).
fn is_map(name: &str) -> bool {
    name == "Map" || name.ends_with("::Map") || name.ends_with(".Map")
}

impl FnLower<'_, '_> {
    /// Write the class object at `place` (static type `ty`) as its dynamic class when that is
    /// a writable descendant; `write_static` writes it as `ty`.
    pub(super) fn json_write_class(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        write_static: impl FnOnce(&mut Self),
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return write_static(self);
        };
        let subs = if self.cx.has_header(d) {
            self.cx.json_descendants(ty)
        } else {
            vec![]
        };
        if subs.is_empty() {
            return write_static(self);
        }
        let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let vt = self.obj_vtable(obj, ty);
        let done = self.new_block();
        for sub in subs {
            let expected = self.vtable_addr(VtableKey::Class(sub));
            let hit = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, vt.clone(), expected));
            let (then, next) = (self.new_block(), self.new_block());
            self.branch(hit, then, next);
            self.switch_to(then);
            // A class value is its object pointer, so `place` is also a `sub`-typed place.
            let a = self.addr(place.clone());
            self.call_glue(Glue::JsonWrite, sub, vec![buf.clone(), a]);
            self.goto(done);
            self.switch_to(next);
        }
        write_static(self);
        self.goto(done);
        self.switch_to(done);
    }
}
