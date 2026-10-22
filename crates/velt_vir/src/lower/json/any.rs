//! `JSON.stringify` of a dynamic value (`any`, `unknown`, `object`; issue #924, hir_encodings.md
//! "any and unknown"), at the top or inside a typed value: one indirect call through slot 14
//! of the descriptor, `AnyJson (data, buf) -> u8`, present only when the program writes one
//! (`Boxing::any_json`). The slot appends the JSON text of the value held with the writer of its
//! concrete type (json/write.rs; a class's `toJSON()` included) and answers `ST_OK`, or answers
//! `ST_UNSUPPORTED` (a `TypeError`) for a value with no JSON form here (a function field, a
//! proxy). A class or record seen again while it is being written is Node's
//! `TypeError: Converting circular structure to JSON`.
//!
//! As in Node, `undefined`, a function and a symbol are left out as an object member (or a
//! record entry) and written as `null` as an array element; at the top, where Node returns
//! `undefined` instead of a string, they are a `TypeError` (`JSON.stringify` returns a
//! `string` in Velt).

use rustc_hash::FxHashSet;

use velt_sema::hir::{self, TyId, TyKind};

use crate::lower::any::{TAG_FUNCTION, TAG_SYMBOL, TAG_UNDEFINED};
use crate::lower::any_member::{ST_OK, ST_UNSUPPORTED};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

pub(in crate::lower) const SLOT_JSON: i128 = 14;

impl FnLower<'_, '_> {
    /// Can the JSON writer (json/write.rs) write a value of the concrete type `t` that a dynamic
    /// value holds? `seen`: the types being checked (a recursive type is fine). `void`: set to
    /// the name of a field of an object type that is `void` after substitution (#1411), which
    /// keeps no value, so what was written to it can't be written.
    fn json_dyn_ok(
        &mut self,
        t: TyId,
        seen: &mut FxHashSet<TyId>,
        void: &mut Option<String>,
    ) -> bool {
        if self.cx.is_dynamic(t) || self.cx.is_json_value(t) || !seen.insert(t) {
            return true;
        }
        if let Some((_, v)) = self.prelude_record(t) {
            return self.json_dyn_ok(v, seen, void);
        }
        if let Some((k, v)) = self.prelude_map(t) {
            return matches!(self.cx.kind(k), TyKind::Str) && self.json_dyn_ok(v, seen, void);
        }
        match self.cx.kind(t) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Literal(_) => true,
            TyKind::Option(e, _) | TyKind::Array(e) | TyKind::Shared(e) => {
                self.json_dyn_ok(e, seen, void)
            }
            TyKind::Tuple(es) => es.into_iter().all(|e| self.json_dyn_ok(e, seen, void)),
            TyKind::Adt(d, _) => match self.cx.hir.def(d) {
                hir::Def::Enum(_) => self.cx.is_c_like_enum(d),
                hir::Def::Adt(a) => {
                    // A class with `toJSON()` (a `Date`) is written as what it returns.
                    if let (Some(m), true) = (a.to_json, a.kind == hir::AdtKind::Class) {
                        let r = self.hook_result(m, t);
                        return self.json_dyn_ok(r, seen, void);
                    }
                    if a.opaque {
                        return false;
                    }
                    let tys = self.cx.adt_field_tys(t);
                    let names: Vec<String> = a.fields.iter().map(|f| f.name.clone()).collect();
                    let anon = a.kind == hir::AdtKind::Anon;
                    names
                        .iter()
                        .zip(tys)
                        .filter(|(n, _)| !velt_sema::property_order::is_hidden_key(n))
                        .all(|(n, f)| {
                            if anon && matches!(self.cx.kind(f), TyKind::Unit) {
                                *void = Some(n.clone());
                                return false;
                            }
                            self.json_dyn_ok(f, seen, void)
                        })
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Does the dynamic value at `place` have a JSON form (not `undefined`, a function or a
    /// symbol)?
    pub(super) fn any_json_present(&mut self, place: &Place) -> Operand {
        let desc = Operand::Copy(proj(place, Proj::Field(1)));
        let tag = self.any_tag_of(desc);
        let out = Place::local(self.temp(Ty::Bool));
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        let cases = [TAG_UNDEFINED, TAG_SYMBOL, TAG_FUNCTION]
            .into_iter()
            .map(|t| (t as i128, no))
            .collect();
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: yes,
        });
        for (b, v) in [(yes, true), (no, false)] {
            self.switch_to(b);
            self.assign(
                out.clone(),
                Rvalue::Use(Operand::Const(Const::Bool(v), Ty::Bool)),
            );
            self.goto(done);
        }
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// `JSON.stringify(x)` of a dynamic `x` at the top: Node's `undefined` for a value without
    /// a JSON form is a `TypeError` here.
    pub(super) fn json_any_top_check(&mut self, place: &Place) {
        let present = self.any_json_present(place);
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(present, ok, bad);
        self.switch_to(bad);
        self.panic_msg("TypeError: `JSON.stringify` of `undefined`, a function or a symbol returns `undefined`, which is not supported through `any` yet (#924)");
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
    }

    /// Append the dynamic value at `place` to the builder at `buf`: `null` for a value without
    /// a JSON form (an array element; object members test `any_json_present` first).
    pub(in crate::lower) fn json_write_any(&mut self, buf: &Operand, place: &Place) {
        self.cx.note_any_json();
        let present = self.any_json_present(place);
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(present, yes, no);
        self.switch_to(no);
        self.push_text(buf, "null");
        self.goto(done);
        self.switch_to(yes);
        let data = Operand::Copy(proj(place, Proj::Field(0)));
        let desc = Operand::Copy(proj(place, Proj::Field(1)));
        let st = self.any_slot(desc, SLOT_JSON, vec![data, buf.clone()], vec![Ty::Ptr; 2]);
        let bad = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st, cint(ST_UNSUPPORTED, Ty::U8)),
        );
        let (stop, fine) = (self.new_block(), self.new_block());
        self.branch(bad, stop, fine);
        self.switch_to(stop);
        self.panic_msg(
            "TypeError: `JSON.stringify` through `any` is not supported for this value yet (#924)",
        );
        self.terminate(Terminator::Unreachable);
        self.switch_to(fine);
        self.goto(done);
        self.switch_to(done);
    }

    /// The body of the JSON slot (module docs) for `ty`.
    pub(in crate::lower) fn any_json_glue(&mut self, data: vir::Local, buf: vir::Local, ty: TyId) {
        self.push_scope(crate::lower::ScopeKind::Block);
        self.any_json_body(data, buf, ty);
        self.scopes.pop();
    }

    fn any_json_body(&mut self, data: vir::Local, buf: vir::Local, ty: TyId) {
        let b = Operand::Copy(Place::local(buf));
        if self.is_proxy(ty) {
            // What Node serializes in its place, read through its traps (`Proxy.__json`).
            let any = self.cx.any_ty();
            let avt = self.cx.ty(any);
            let res = Place::local(self.temp(avt));
            let r = self.proxy_receiver(data, ty);
            if !self.proxy_invoke("__json", data, ty, vec![(r, any)], &res) {
                return self.ret_status(ST_UNSUPPORTED);
            }
            self.json_write_any(&b, &res);
            self.drop_glue(res, any);
            return self.ret_status(ST_OK);
        }
        let mut void = None;
        if !self.json_dyn_ok(ty, &mut FxHashSet::default(), &mut void) {
            if let Some(name) = void {
                self.void_field_panic(&name);
            }
            return self.ret_status(ST_UNSUPPORTED);
        }
        if matches!(self.cx.kind(ty), TyKind::Option(..) | TyKind::Unit) {
            // The dynamic `null`.
            self.push_text(&b, "null");
            return self.ret_status(ST_OK);
        }
        let vp = self.any_value_place(data, ty);
        // An object, an array or a tuple may hold itself through a dynamic value (the data
        // pointer is its identity); a recursive object type's own writer tests that already.
        let guard = match self.cx.kind(ty) {
            TyKind::Adt(d, _) => !self.cx.recursive_object(d),
            TyKind::Array(_) | TyKind::Tuple(_) | TyKind::Shared(_) => true,
            _ => false,
        };
        if guard {
            let ok = self.rt_u8(Rt::JsonEnter, vec![Operand::Copy(Place::local(data))]);
            let (cycle, fine) = (self.new_block(), self.new_block());
            self.branch(ok, fine, cycle);
            self.switch_to(cycle);
            self.panic_msg("TypeError: Converting circular structure to JSON");
            self.terminate(Terminator::Unreachable);
            self.switch_to(fine);
        }
        self.json_write(&b, &vp, ty);
        if guard {
            self.call_rt(Rt::JsonLeave, vec![], None);
        }
        self.ret_status(ST_OK);
    }
}
