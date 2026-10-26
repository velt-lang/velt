//! Member writes on `any` (phase 1b of #924; hir_encodings.md "any and unknown"):
//! `Intrinsic::AnySet` / `AnySetIndex`, one indirect call through a descriptor slot of the
//! value held, like the reads of any_member.rs. The tables have these slots only when the
//! program has such a write (`Boxing::any_writes`):
//!
//! | slot | entry |
//! |---|---|
//! | 9 | `AnySet`: `(data, key: *string, v: *any) -> u8` |
//! | 10 | `AnySetIndex`: `(data, i: f64, v: *any) -> u8` |
//!
//! The slot owns `*v`: it converts it to the member's type with the checked conversion of
//! `AnyTo` (a `TypeError` for a value of another type) and stores it, dropping the old value,
//! or passes it to the member's setter; otherwise it drops it. Besides `ST_OK` and
//! `ST_UNSUPPORTED` the status is `ST_MISSING`: the fixed shape has no such member (Node would
//! add it). A getter without a setter stops in the slot itself with Node's `TypeError`.
//!
//! What each concrete type takes: fields of class instances and object types (not `readonly`
//! ones, nor optional fields that keep a present `null` apart), setters, keys of a
//! `Record<string, V>` (added when missing, as in Node), and elements of an array inside its
//! length or just past it (the array grows by one), and an array's `length` made shorter.

use velt_sema::hir::{DynMember, DynMemberKind, LitValue, TyId, TyKind};

use super::any_glue::OBJECT_PROTO;
use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::any_proxy::ProxyKey;
use super::glue::Glue;
use super::operand::proj;
use super::FnLower;
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

pub(super) const SLOT_SET: i128 = 9;
pub(super) const SLOT_SET_INDEX: i128 = 10;

/// The member is not part of the value's fixed shape.
pub(super) const ST_MISSING: i128 = 3;

impl FnLower<'_, '_> {
    /// The body of write glue `g` (module docs) for `ty`.
    pub(super) fn any_set_glue(&mut self, g: Glue, args: &[vir::Local], ty: TyId) {
        self.push_scope(super::ScopeKind::Block);
        match (g, args) {
            (Glue::AnySet, &[d, k, v]) => self.any_set_body(d, k, v, ty),
            (Glue::AnySetIndex, &[d, i, v]) => self.any_set_index_body(d, i, v, ty),
            _ => super::ice("write glue with the wrong parameters"),
        }
        self.scopes.pop();
    }

    /// Drop the `any` at `*v` and return `status`.
    fn any_set_refuse(&mut self, v: vir::Local, status: i128) {
        let any = self.cx.any_ty();
        let vp = self.any_out(v);
        self.drop_glue(vp, any);
        self.ret_status(status);
    }

    /// The value of type `t` the `any` at `*v` holds (taken from it), checked.
    fn any_set_value(&mut self, v: vir::Local, t: TyId) -> Operand {
        let any = self.cx.any_ty();
        let vp = self.any_out(v);
        let avt = self.cx.ty(any);
        let tmp = Place::local(self.copy_to_temp(Operand::Copy(vp), avt));
        self.any_unbox(&tmp, any, t)
    }

    /// Store the owned value `val` of type `t` at `p`, dropping the value it replaces (after
    /// the store, so whatever the drop runs sees the new value).
    pub(super) fn any_replace(&mut self, p: Place, val: Operand, t: TyId) {
        let vt = self.cx.ty(t);
        let old = Place::local(self.copy_to_temp(Operand::Copy(p.clone()), vt));
        self.store(p, val);
        self.drop_glue(old, t);
    }

    fn any_set_body(&mut self, data: vir::Local, key: vir::Local, v: vir::Local, ty: TyId) {
        if self.is_proxy(ty) {
            return self.proxy_set_body(data, ProxyKey::Str(key), v, ty);
        }
        if let Some((k, val)) = self.prelude_record(ty) {
            return self.any_set_record(data, key, v, ty, (k, val));
        }
        if let TyKind::Array(elem) = self.cx.kind(ty) {
            let next = self.on_key(key, "length");
            self.any_set_array_length(data, v, ty, elem);
            self.switch_to(next);
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        }
        let Some((d, _)) = self.plain_adt(ty) else {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        };
        // A value struct held by an `any` is a copy: writing it would not reach the original.
        if !(self.cx.is_class(ty) || self.cx.is_object(ty)) {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        }
        self.any_subclass_dispatch(Glue::AnySet, ty, d, &[data, key, v]);
        let members = self.dyn_members_of(d);
        let readonly: Vec<String> = members
            .iter()
            .filter(|m| m.kind == DynMemberKind::ReadonlyField)
            .map(|m| m.name.clone())
            .collect();
        let fields = self.member_fields(d);
        let vp = self.any_value_place(data, ty);
        for (i, name) in fields.iter().enumerate() {
            if name.starts_with('#') {
                continue;
            }
            let next = self.on_key(key, name);
            let fty = self.cx.member_ty(ty, i as u32);
            if readonly.contains(name) || !self.cx.any_unboxable(fty) {
                self.any_set_refuse(v, ST_UNSUPPORTED);
            } else {
                let val = self.any_set_value(v, fty);
                let fp = self.field_place(&vp, ty, i as u32);
                // An optional field that keeps "absent" apart (`a?: T`) is present once written.
                if let Some(slot) = self.cx.presence_slot(ty, i as u32) {
                    let mut flag = fp.clone();
                    if let Some(crate::vir::Proj::Field(x)) = flag.proj.last_mut() {
                        *x = slot;
                    }
                    self.assign(flag, crate::vir::Rvalue::Use(Self::ctrue()));
                }
                self.any_replace(fp, val, fty);
                self.ret_status(ST_OK);
            }
            self.switch_to(next);
        }
        let setters: Vec<String> = members
            .iter()
            .filter(|m| m.kind == DynMemberKind::Setter)
            .map(|m| m.name.clone())
            .collect();
        for m in &members {
            if m.kind == DynMemberKind::ReadonlyField || fields.contains(&m.name) {
                continue;
            }
            // A getter whose setter is listed too is handled with the setter.
            if m.kind == DynMemberKind::Getter && setters.contains(&m.name) {
                continue;
            }
            let next = self.on_key(key, &m.name);
            match (m.kind, self.ready(ty, m)) {
                (DynMemberKind::Setter, Some(r)) => {
                    let f = self.cx.fn_def(r.def);
                    let pt = f.params.get(1).map(|p| p.ty);
                    match pt.map(|t| self.cx.subst(t, &r.targs)) {
                        Some(pt) if self.cx.any_unboxable(pt) => {
                            let val = self.any_set_value(v, pt);
                            let vt = self.cx.ty(pt);
                            let slot = Place::local(self.temp(vt));
                            self.store(slot.clone(), val);
                            let this = self.any_this(data, ty);
                            let any = self.cx.any_ty();
                            let avt = self.cx.ty(any);
                            let out = Place::local(self.temp(avt));
                            self.any_invoke(&r, this, vec![(slot, pt)], &out);
                            self.drop_glue(out, any);
                            self.ret_status(ST_OK);
                        }
                        _ => self.any_set_refuse(v, ST_UNSUPPORTED),
                    }
                }
                (DynMemberKind::Getter, _) if self.cx.is_class(ty) => {
                    // Class bodies are strict code: Node throws.
                    let name = self.cx.type_name(ty);
                    self.panic_msg(&format!(
                        "TypeError: Cannot set property {} of #<{name}> which has only a getter",
                        m.name
                    ));
                    self.terminate(Terminator::Unreachable);
                }
                // A method replaced by a value, and the rest, come later.
                _ => self.any_set_refuse(v, ST_UNSUPPORTED),
            }
            self.switch_to(next);
        }
        for name in OBJECT_PROTO {
            let next = self.on_key(key, name);
            self.any_set_refuse(v, ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_set_refuse(v, ST_MISSING);
    }

    /// The fields of struct `d` that hold an object literal's accessors as functions (the
    /// getter `get k()` in field `k`, the setter in field `set k`), which sema lists as its
    /// getters and setters (`Program::dyn_members`): through `any` the accessor runs, and the
    /// field is no member of its own (#1538).
    pub(super) fn accessor_fields(&mut self, d: velt_sema::hir::DefId) -> Vec<String> {
        let mut out = vec![];
        for m in self.dyn_members_of(d) {
            match m.kind {
                DynMemberKind::Getter => out.push(m.name),
                DynMemberKind::Setter => out.push(format!("set {}", m.name)),
                _ => {}
            }
        }
        out
    }

    /// Is `m` a getter or setter of an object literal, held as a function in a field of its
    /// struct `owner` (`accessor_fields`)? Such an accessor may throw (the setter
    /// `Object.defineProperty` adds, before the call has run): its error then ends the process
    /// as uncaught (proxy_throws.rs).
    pub(super) fn literal_accessor(&self, owner: TyId, m: &DynMember) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(owner) else {
            return false;
        };
        let held = match m.kind {
            DynMemberKind::Getter => m.name.clone(),
            DynMemberKind::Setter => format!("set {}", m.name),
            _ => return false,
        };
        self.cx.adt_def(d).fields.iter().any(|f| f.name == held)
    }

    /// The names of struct `d`'s fields as members through `any`, in order: a field holding an
    /// accessor (`accessor_fields`) is named `#` first, as a private field is, so it is no member
    /// of its own and the accessor is found instead.
    pub(super) fn member_fields(&mut self, d: velt_sema::hir::DefId) -> Vec<String> {
        let shadowed = self.accessor_fields(d);
        let fields = self.cx.adt_def(d).fields.iter().map(|f| f.name.clone());
        fields
            .map(|n| match shadowed.contains(&n) {
                true => format!("#{n}"),
                false => n,
            })
            .collect()
    }

    /// `r[key] = v` on a `Record<string, V>`: its `__set`.
    fn any_set_record(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        v: vir::Local,
        ty: TyId,
        (k, val): (TyId, TyId),
    ) {
        let Some((d, _)) = self.plain_adt(ty) else {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        };
        let set = self
            .dyn_members_of(d)
            .into_iter()
            .find(|m| m.name == "__set" && m.kind == DynMemberKind::Method);
        let ready = set.and_then(|m| self.ready(ty, &m));
        let str_key = matches!(self.cx.kind(k), TyKind::Str);
        let Some(r) = ready.filter(|_| str_key && self.cx.any_unboxable(val)) else {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        };
        let x = self.any_set_value(v, val);
        let vt = self.cx.ty(val);
        let slot = Place::local(self.temp(vt));
        self.store(slot.clone(), x);
        let kvt = self.cx.ty(k);
        let kp = self.deref_param(key, k);
        let ks = self.share_value(Operand::Copy(kp), k);
        let kslot = Place::local(self.copy_to_temp(ks, kvt));
        let this = self.any_this(data, ty);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        self.any_invoke(&r, this, vec![(kslot, k), (slot, val)], &out);
        self.drop_glue(out, any);
        self.ret_status(ST_OK);
    }

    /// `a.length = v` on an array: a shorter length drops the elements past it, as in Node; an
    /// invalid length is Node's `RangeError`; a longer one (holes) is not supported yet.
    fn any_set_array_length(&mut self, data: vir::Local, v: vir::Local, ty: TyId, elem: TyId) {
        let f64_ = self.cx.intern(TyKind::Float(velt_sema::hir::FloatTy::F64));
        let n = self.any_set_value(v, f64_);
        let n = self.rvalue_temp(Ty::F64, Rvalue::Use(n));
        let vp = self.any_value_place(data, ty);
        let arr = self.content(&vp, ty);
        let idx = self.rvalue_temp(Ty::U64, Rvalue::Cast(n.clone(), Ty::U64));
        let back = self.rvalue_temp(Ty::F64, Rvalue::Cast(idx.clone(), Ty::F64));
        let whole = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, n));
        let (ok, bad) = (self.new_block(), self.new_block());
        self.branch(whole, ok, bad);
        self.switch_to(bad);
        self.panic_msg("RangeError: Invalid array length");
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        let len = Operand::Copy(proj(&arr, Proj::Field(1)));
        let fits = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, idx.clone(), len));
        let (cut, grow) = (self.new_block(), self.new_block());
        self.branch(fits, cut, grow);
        self.switch_to(cut);
        self.truncate(&arr, elem, idx);
        self.ret_status(ST_OK);
        self.switch_to(grow);
        self.ret_status(ST_UNSUPPORTED);
    }

    fn any_set_index_body(&mut self, data: vir::Local, i: vir::Local, v: vir::Local, ty: TyId) {
        if self.is_proxy(ty) {
            return self.proxy_set_body(data, ProxyKey::Num(i), v, ty);
        }
        let TyKind::Array(elem) = self.cx.kind(ty) else {
            // A number key names no field of a fixed shape; other values come later.
            let st = if self.plain_adt(ty).is_some() && self.prelude_record(ty).is_none() {
                ST_MISSING
            } else {
                ST_UNSUPPORTED
            };
            return self.any_set_refuse(v, st);
        };
        if !self.cx.any_unboxable(elem) {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        }
        let vp = self.any_value_place(data, ty);
        let arr = self.content(&vp, ty);
        let iv = Operand::Copy(Place::local(i));
        let idx = self.rvalue_temp(Ty::U64, Rvalue::Cast(iv.clone(), Ty::U64));
        let back = self.rvalue_temp(Ty::F64, Rvalue::Cast(idx.clone(), Ty::F64));
        let whole = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, iv));
        let len = Operand::Copy(proj(&arr, Proj::Field(1)));
        let len2 = len.clone();
        let inside = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, idx.clone(), len));
        let (w, yes, no) = (self.new_block(), self.new_block(), self.new_block());
        let w_end = self.new_block();
        self.branch(whole, w, no);
        self.switch_to(w);
        self.branch(inside, yes, w_end);
        self.switch_to(yes);
        let val = self.any_set_value(v, elem);
        let ep = self.elem_place(&arr, idx.clone(), elem);
        self.any_replace(ep, val, elem);
        self.ret_status(ST_OK);
        // At the end: the array grows by one, as in Node (`Array.prototype.push` through a
        // proxy writes there).
        self.switch_to(w_end);
        let at_end = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, idx, len2));
        let (push, no2) = (self.new_block(), self.new_block());
        self.branch(at_end, push, no2);
        self.switch_to(push);
        let val = self.any_set_value(v, elem);
        self.push_value(&arr, elem, val);
        self.ret_status(ST_OK);
        // Past the end (Node grows the array, with holes) or not an index (Node adds a
        // property): later.
        self.switch_to(no);
        self.any_set_refuse(v, ST_UNSUPPORTED);
        self.switch_to(no2);
        self.any_set_refuse(v, ST_UNSUPPORTED);
    }
}

impl super::Cx<'_> {
    /// Can glue convert an `any` to a value of `t` (`any_unbox`)? Unions, literal and enum
    /// types (which sema converts member by member) and interface values are not reached yet.
    pub(super) fn any_unboxable(&self, t: TyId) -> bool {
        !matches!(self.types.kind(t), TyKind::Unit) && self.any_boxable(t)
    }
}

impl FnLower<'_, '_> {
    /// The value of the union type `ty` (`string | symbol`, boxable member by member,
    /// `Cx::any_boxable`) the `any` at `p` (owned, consumed here) holds: the member whose type
    /// the value has, else a `TypeError` naming the union.
    pub(super) fn any_unbox_union(&mut self, p: &Place, from: TyId, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        let out = Place::local(self.temp(vt));
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let done = self.new_block();
        let n = match self.cx.kind(ty) {
            TyKind::Adt(d, _) => self.cx.enum_def(d).variants.len() as u32,
            _ => 0,
        };
        for v in 0..n {
            let [pt] = self.cx.variant_tys(ty, v)[..] else {
                continue;
            };
            let ok = match self.cx.kind(pt) {
                TyKind::Adt(class, args) if self.cx.is_class(pt) && args.is_empty() => {
                    let tag = self.any_tag_of(desc.clone());
                    let obj = super::cint(super::any::TAG_OBJECT as i128, Ty::U64);
                    let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, obj));
                    let r = Place::local(self.temp(Ty::Bool));
                    let (test, no, join) = (self.new_block(), self.new_block(), self.new_block());
                    self.branch(is_obj, test, no);
                    self.switch_to(test);
                    let t = self.instance_test(p, from, class);
                    self.assign(r.clone(), Rvalue::Use(t));
                    self.goto(join);
                    self.switch_to(no);
                    self.assign(
                        r.clone(),
                        Rvalue::Use(Operand::Const(vir::Const::Bool(false), Ty::Bool)),
                    );
                    self.goto(join);
                    self.switch_to(join);
                    Operand::Copy(r)
                }
                // A string literal member (`"a" | "b"`, a key type known per instantiation):
                // the value held is a string with that content.
                TyKind::Literal(LitValue::Str(lit)) => {
                    let imp = self.cx.hir.any_impl.unwrap_or(0);
                    let str_ty = self.cx.str_ty();
                    let want = self.vtable_addr(super::glue::VtableKey::Impl(imp, str_ty));
                    let is_str =
                        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want));
                    let r = Place::local(self.temp(Ty::Bool));
                    let (test, no, join) = (self.new_block(), self.new_block(), self.new_block());
                    self.branch(is_str, test, no);
                    self.switch_to(test);
                    let s = self.str_lit(&lit);
                    let sa = self.operand_addr(s, Ty::Agg(vir::STR_AGG));
                    let data = Operand::Copy(proj(p, Proj::Field(0)));
                    let eq = self.str_eq(data, sa);
                    self.assign(r.clone(), Rvalue::Use(eq));
                    self.goto(join);
                    self.switch_to(no);
                    self.assign(
                        r.clone(),
                        Rvalue::Use(Operand::Const(vir::Const::Bool(false), Ty::Bool)),
                    );
                    self.goto(join);
                    self.switch_to(join);
                    Operand::Copy(r)
                }
                _ => {
                    let imp = self.cx.hir.any_impl.unwrap_or(0);
                    let want = self.vtable_addr(super::glue::VtableKey::Impl(imp, pt));
                    self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want))
                }
            };
            let (yes, next) = (self.new_block(), self.new_block());
            self.branch(ok, yes, next);
            self.switch_to(yes);
            // A zero-sized member (a literal type) holds nothing: the box is released.
            let val = match self.cx.is_unit(pt) {
                true => {
                    self.drop_glue(p.clone(), from);
                    super::unit()
                }
                false => self.any_unbox(p, from, pt),
            };
            let view = self.cx.view(ty, v);
            let vp = proj(&out, Proj::Cast(view));
            self.assign(
                proj(&vp, Proj::Field(0)),
                Rvalue::Use(super::cint(v as i128, Ty::U32)),
            );
            if !self.cx.is_unit(pt) {
                self.assign(proj(&vp, Proj::Field(1)), Rvalue::Use(val));
            }
            self.goto(done);
            self.switch_to(next);
        }
        // An object or array of another type: a view of it as the union's member.
        self.union_array_view(p, desc.clone(), ty, n, &out, done);
        self.union_object_view(p, desc.clone(), ty, n, &out, done);
        self.any_type_error(desc, ty);
        self.switch_to(done);
        Operand::Copy(out)
    }
}
