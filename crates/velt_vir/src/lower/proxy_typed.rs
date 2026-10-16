//! Typed proxies (issue #823; hir_encodings.md "Proxy and Reflect"). A proxy starts as an
//! `any` holding the prelude's `Proxy` object (any_proxy.rs). Converting it to a typed value
//! keeps that object: the typed value is the object's address with bit 0 set (counted objects
//! are 8-aligned, so the bit is free). Nothing is copied, so identity is the same whatever the
//! static type.
//!
//! Only the *proxiable* types can hold such a pointer: the class hierarchies and object types
//! that an `any` is converted to (`AnyTo`) in the program's own code, in a program that
//! creates a proxy, and what values of those types are upcast to. A program without
//! `new Proxy` has none, and its code is unchanged. On a proxiable type:
//!
//! | operation | bit 0 clear | bit 0 set (cold, outlined per field type) |
//! |---|---|---|
//! | `s.f` | the load | `__get("f", receiver)` checked as the field's type (`Glue::ProxyGet`) |
//! | `s.f = v` | the store | `__set("f", v, receiver)`; `false` is Node's `TypeError` (`Glue::ProxySet`) |
//! | `s.f op= v` | both of the above | both of the above |
//! | `s.m()` | the call, `this` = `s` | the call with `this` = the proxy, so `this.x` runs the traps |
//! | retain / release | the count | the `Proxy` object's count (the bit cleared first) |
//! | drop | `S`'s drop | the `Proxy` object's drop |
//! | to `any` | `S`'s table | the `Proxy` object with its own table |
//! | `console.log(s)` | `S`'s format | the proxy's (its target, no trap runs) |
//!
//! The receiver the traps see is the proxy itself, as an `any`.

use std::collections::HashSet;

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::any_proxy::{ProxyKey, PRELUDE_PROXY};
use super::boxing::Boxing;
use super::operand::proj;
use super::{cint, Cx, FnLower, Glue, Work};
use crate::vir::{self, BinOp, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// May a value of the concrete type `t` hold a tagged proxy?
    pub(super) fn proxiable(&self, t: TyId) -> bool {
        let b = &self.boxing;
        if b.proxy_roots.is_empty() && b.proxy_types.is_empty() {
            return false;
        }
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => b.proxy_roots.contains(&self.class_root(*d)),
            _ => b.proxy_types.contains(&t),
        }
    }

    /// The prelude's `Proxy` class type (only asked for in a program with proxiable types).
    pub(super) fn proxy_class_ty(&mut self) -> TyId {
        let d = self
            .boxing
            .proxy_class
            .unwrap_or_else(|| super::ice("a typed proxy without the `Proxy` class"));
        self.intern(TyKind::Adt(d, vec![]))
    }

    /// Record that the program constructs a `Proxy`.
    pub(super) fn note_proxy_new(&mut self) {
        self.facts.proxy_new = true;
    }

    /// Record an `AnyTo` to the concrete type `t` (rule 1 of #823's design: in a program that
    /// creates a proxy, the object types an `any` is converted to may hold one). Conversions
    /// in the standard library are not counted.
    pub(super) fn note_proxy_target(&mut self, t: TyId) {
        let std = self.cur_def.is_some_and(|d| match self.hir.def(d) {
            hir::Def::Fn(f) => f.name.starts_with("std/"),
            _ => false,
        });
        if !std {
            self.facts.proxy_targets.insert(t);
        }
    }

    /// Record an upcast from `from` to `to` (rule 2: a proxiable `from` makes `to` proxiable).
    pub(super) fn note_proxy_cast(&mut self, from: TyId, to: TyId) {
        if from != to {
            self.facts.proxy_casts.insert((from, to));
        }
    }

    /// The proxiable types implied by this pass's facts (rules 1 to 3 of #823's design), and
    /// the counts they need.
    pub(super) fn close_proxies(&mut self, next: &mut Boxing) {
        if !self.facts.proxy_new {
            return;
        }
        if next.proxy_class.is_none() {
            next.proxy_class = (0..self.hir.defs.len() as u32)
                .map(DefId)
                .find(|d| matches!(self.hir.def(*d), hir::Def::Adt(a) if a.name == PRELUDE_PROXY));
        }
        let Some(proxy) = next.proxy_class else {
            return;
        };
        let targets: Vec<TyId> = self.facts.proxy_targets.iter().copied().collect();
        for t in targets {
            self.add_proxiable(next, t, proxy);
        }
        // Rule 2, to a fixed point: what a proxiable value is upcast to.
        let casts: Vec<(TyId, TyId)> = self.facts.proxy_casts.iter().copied().collect();
        let mut seen = HashSet::new();
        loop {
            let mut changed = false;
            for &(from, to) in &casts {
                if !seen.contains(&(from, to)) && self.proxiable_in(next, from) {
                    seen.insert((from, to));
                    self.add_proxiable(next, to, proxy);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Make `t` proxiable in `next` when it is a class (rule 3: its whole hierarchy) or an
    /// object type; either is counted.
    fn add_proxiable(&mut self, next: &mut Boxing, t: TyId, proxy: DefId) {
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => {
                if *d == proxy {
                    return;
                }
                let root = self.class_root(*d);
                next.proxy_roots.insert(root);
                next.roots.insert(root);
            }
            TyKind::Adt(d, _) => {
                if matches!(self.hir.def(*d), hir::Def::Adt(a) if a.kind == AdtKind::Anon) {
                    next.proxy_types.insert(t);
                    next.boxes.insert(t);
                }
            }
            _ => {}
        }
    }

    fn proxiable_in(&self, b: &Boxing, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => b.proxy_roots.contains(&self.class_root(*d)),
            _ => b.proxy_types.contains(&t),
        }
    }

    /// Display names of the proxiable types (`VELT_DEBUG_PROXY=1`).
    pub(super) fn proxiable_names(&self) -> Vec<String> {
        let roots = self.boxing.proxy_roots.iter().map(|d| match self.hir.def(*d) {
            hir::Def::Adt(a) => format!("class {} (and its subclasses)", a.name),
            _ => "?".into(),
        });
        let types = self.boxing.proxy_types.iter().map(|&t| match self.types.kind(t) {
            TyKind::Adt(d, _) => match self.hir.def(*d) {
                hir::Def::Adt(a) => a.name.clone(),
                _ => self.type_name(t),
            },
            _ => self.type_name(t),
        });
        let mut out: Vec<String> = roots.chain(types).collect();
        out.sort();
        out
    }
}

impl FnLower<'_, '_> {
    /// Is bit 0 of the pointer `p` set (a tagged proxy)?
    pub(super) fn proxy_tagged(&mut self, p: Operand) -> Operand {
        let w = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let b = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, w, cint(1, Ty::U64)),
        );
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, b, cint(0, Ty::U64)))
    }

    /// The pointer `p` with bit 0 cleared.
    pub(super) fn proxy_untag(&mut self, p: Operand) -> Operand {
        let w = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let c = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, w, cint(-2, Ty::U64)),
        );
        self.rvalue_temp(Ty::Ptr, Rvalue::Cast(c, Ty::Ptr))
    }

    /// The pointer `p` with bit 0 set.
    pub(super) fn proxy_tag(&mut self, p: Operand) -> Operand {
        let w = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let c = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitOr, w, cint(1, Ty::U64)));
        self.rvalue_temp(Ty::Ptr, Rvalue::Cast(c, Ty::Ptr))
    }

    /// The pointer to count for a value `p` of type `ty`: the bit cleared on a proxiable type.
    pub(super) fn count_ptr(&mut self, p: Operand, ty: TyId) -> Operand {
        match self.cx.proxiable(ty) {
            true => self.proxy_untag(p),
            false => p,
        }
    }

    /// Branch on the tag of the pointer `p`: `cold` runs with the untagged pointer, `hot` with
    /// `p`; both continue in the returned join block's position.
    fn proxy_split(
        &mut self,
        p: &Operand,
        cold: impl FnOnce(&mut Self, Operand),
        hot: impl FnOnce(&mut Self),
    ) {
        let t = self.proxy_tagged(p.clone());
        let (c, h, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(t, c, h);
        self.switch_to(c);
        let u = self.proxy_untag(p.clone());
        cold(self, u);
        if !self.dead() {
            self.goto(join);
        }
        self.switch_to(h);
        hot(self);
        if !self.dead() {
            self.goto(join);
        }
        self.switch_to(join);
    }

    /// The `any` table of the `Proxy` class.
    fn proxy_table(&mut self) -> Operand {
        let pty = self.cx.proxy_class_ty();
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        self.vtable_addr(super::glue::VtableKey::Impl(imp, pty))
    }

    /// `(data, table)` of the `any` for the value `v` (a class object or box) of the
    /// proxiable type `t`: a tagged proxy gives the `Proxy` object with its own table.
    pub(super) fn proxy_any_parts(&mut self, v: Operand, own: Operand) -> (Operand, Operand) {
        let (d, t) = (self.temp(Ty::Ptr), self.temp(Ty::Ptr));
        let v = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
        self.proxy_split(
            &v,
            |lw, u| {
                lw.assign(Place::local(d), Rvalue::Use(u));
                let pt = lw.proxy_table();
                lw.assign(Place::local(t), Rvalue::Use(pt));
            },
            |lw| {
                lw.assign(Place::local(d), Rvalue::Use(v.clone()));
                lw.assign(Place::local(t), Rvalue::Use(own));
            },
        );
        (Operand::Copy(Place::local(d)), Operand::Copy(Place::local(t)))
    }

    /// In `AnyTo` to the proxiable `ty`: when the descriptor `desc` is the `Proxy` class's,
    /// the tagged object into `out` and on to `done`; otherwise on to `no`.
    pub(super) fn proxy_unbox(
        &mut self,
        data: Operand,
        desc: Operand,
        out: &Place,
        done: vir::BlockId,
        no: vir::BlockId,
    ) {
        let pt = self.proxy_table();
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, pt));
        let yes = self.new_block();
        self.branch(is, yes, no);
        self.switch_to(yes);
        let tagged = self.proxy_tag(data);
        self.assign(out.clone(), Rvalue::Use(tagged));
        self.goto(done);
    }

    /// The name and type of field `index` of the proxiable `bty`.
    fn proxy_field(&mut self, bty: TyId, index: u32) -> (String, TyId) {
        let TyKind::Adt(d, _) = self.cx.kind(bty) else {
            super::ice("a proxiable type that is not an object type")
        };
        let name = self.cx.adt_def(d).fields[index as usize].name.clone();
        (name, self.cx.member_ty(bty, index))
    }

    /// The field place of `index` from the (untagged) pointer `ptr` to a `bty` value.
    fn proxy_hot_place(&mut self, ptr: &Operand, bty: TyId, index: u32) -> Place {
        let pp = self.operand_place(ptr.clone(), Ty::Ptr);
        let agg = match self.cx.is_class(bty) {
            true => Ty::Agg(self.cx.obj_agg(bty)),
            false => self.cx.payload_ty(bty),
        };
        let f = self.cx.vir_field(bty, index);
        proj(&proj(&pp, Proj::Deref(agg)), Proj::Field(f))
    }

    /// Stop with Node's error for a private name read through a proxy.
    fn proxy_private(&mut self, name: &str) {
        self.panic_msg(&format!(
            "TypeError: Cannot read private member {name} from an object whose class did not declare it"
        ));
        self.terminate(Terminator::Unreachable);
    }

    /// The key `name` as a static string, by address.
    fn proxy_key_addr(&mut self, name: &str) -> Operand {
        let s = self.str_lit(name);
        self.operand_addr(s, Ty::Agg(vir::STR_AGG))
    }

    /// `base.f` where `base` has the proxiable type `bty`: the field's place (borrowed), read
    /// through the `get` trap when `base` holds a proxy.
    pub(super) fn proxy_field_read(
        &mut self,
        base: &hir::Expr,
        bty: TyId,
        index: u32,
        mode: hir::UseMode,
    ) -> Operand {
        let v = self.expr(base);
        let bp = self.place_of(v, bty);
        let hop = self.retained_hop(&bp, bty);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(hop)));
        let place = self.proxy_read_at(&ptr, bty, index);
        let (_, fty) = self.proxy_field(bty, index);
        if mode == hir::UseMode::Move {
            let s = self.share_value(Operand::Copy(place), fty);
            return self.own_value(s, fty);
        }
        Operand::Copy(place)
    }

    /// The value of field `index` of the `bty` value at `ptr`, in a temporary (borrowed; a
    /// proxy's answer is owned by a slot dropped with the statement's temporaries).
    fn proxy_read_at(&mut self, ptr: &Operand, bty: TyId, index: u32) -> Place {
        let (name, fty) = self.proxy_field(bty, index);
        let fvt = self.cx.ty(fty);
        let val = self.temp(fvt);
        // A value that owns something lands in a slot of its own, dropped later; the slot is
        // zero (nothing to drop) when the field was read directly.
        let slot = self.cx.needs_drop(fty).then(|| {
            let slot = self.temp(fvt);
            let zero = self.zero_value(fvt);
            self.assign(Place::local(slot), Rvalue::Use(zero));
            self.own_temp(slot, fty);
            slot
        });
        self.proxy_split(
            ptr,
            |lw, u| {
                if name.starts_with('#') {
                    return lw.proxy_private(&name);
                }
                let key = lw.proxy_key_addr(&name);
                let out = lw.addr(Place::local(slot.unwrap_or(val)));
                let f = lw.cx.func(Work::Glue(Glue::ProxyGet, fty));
                lw.call(Callee::Func(f), vec![u, key, out], None, false);
                if let Some(slot) = slot {
                    lw.assign(Place::local(val), Rvalue::Use(Operand::Copy(Place::local(slot))));
                }
            },
            |lw| {
                let fp = lw.proxy_hot_place(ptr, bty, index);
                lw.assign(Place::local(val), Rvalue::Use(Operand::Copy(fp)));
            },
        );
        Place::local(val)
    }

    /// The base of a write to a field of the proxiable `bty`, held (shared) for the statement
    /// when the right-hand side `value` may write memory (and so drop the object).
    fn proxy_hold(&mut self, base: &hir::Expr, bty: TyId, value: &hir::Expr) -> Operand {
        let v = self.expr(base);
        if !super::sequence::may_write(value) {
            return self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
        }
        let s = self.share_value(v, bty);
        let o = self.own_value(s, bty);
        self.rvalue_temp(Ty::Ptr, Rvalue::Use(o))
    }

    /// Store the owned `v` into field `index` of the `bty` value at `ptr`, through the `set`
    /// trap when it holds a proxy. `place` is the HIR target (for a presence flag).
    fn proxy_write_at(
        &mut self,
        ptr: &Operand,
        bty: TyId,
        index: u32,
        place: &hir::Expr,
        v: Operand,
    ) {
        let (name, fty) = self.proxy_field(bty, index);
        let fvt = self.cx.ty(fty);
        let val = self.copy_to_temp(v, fvt);
        let drops = self.cx.needs_drop(fty);
        self.proxy_split(
            ptr,
            |lw, u| {
                if name.starts_with('#') {
                    return lw.proxy_private(&name);
                }
                let key = lw.proxy_key_addr(&name);
                let vp = lw.addr(Place::local(val));
                let f = lw.cx.func(Work::Glue(Glue::ProxySet, fty));
                lw.call(Callee::Func(f), vec![u, key, vp], None, false);
            },
            |lw| {
                let fp = lw.proxy_hot_place(ptr, bty, index);
                let v = Operand::Copy(Place::local(val));
                lw.store_assigned(place, fp, v, fty, true, drops);
            },
        );
    }

    /// `base.f = value` on the proxiable `bty`.
    pub(super) fn proxy_assign(
        &mut self,
        place: &hir::Expr,
        base: &hir::Expr,
        index: u32,
        value: &hir::Expr,
    ) -> Operand {
        let bty = self.sub(base.ty);
        let ptr = self.proxy_hold(base, bty, value);
        let v = self.consume(value);
        if self.dead() {
            return super::unit();
        }
        let v = self.detach(v, place.ty);
        self.proxy_write_at(&ptr, bty, index, place, v);
        super::unit()
    }

    /// `base.f op= value` on the proxiable `bty`: the read, the right-hand side, the write.
    pub(super) fn proxy_compound(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        (base, index): (&hir::Expr, u32),
        value: &hir::Expr,
    ) -> Operand {
        let bty = self.sub(base.ty);
        let ptr = self.proxy_hold(base, bty, value);
        let (_, fty) = self.proxy_field(bty, index);
        let old = self.proxy_read_at(&ptr, bty, index);
        let fvt = self.cx.ty(fty);
        let old = self.copy_to_temp(Operand::Copy(old), fvt);
        let r = self.expr(value);
        if self.dead() {
            return super::unit();
        }
        let v = self.binop(op, Operand::Copy(Place::local(old)), r, place.ty, place.ty);
        let v = match self.cx.needs_drop(fty) {
            true => self.take_owned(v),
            false => v,
        };
        self.proxy_write_at(&ptr, bty, index, place, v);
        super::unit()
    }

    /// The vtable of the class object `obj` of the proxiable class `cls` for a virtual call: a
    /// proxy's comes from its target (an instance of the class, or a `TypeError`).
    pub(super) fn proxy_obj_vtable(&mut self, obj: Operand, cls: TyId) -> Operand {
        let out = self.temp(Ty::Ptr);
        self.proxy_split(
            &obj,
            |lw, u| {
                let pty = lw.cx.proxy_class_ty();
                let any = lw.cx.any_ty();
                let target = lw.proxy_hot_place(&u, pty, 0);
                let TyKind::Adt(d, _) = lw.cx.kind(cls) else {
                    super::ice("a virtual call on a non-class")
                };
                let ok = lw.instance_test(&target, any, d);
                let (yes, no) = (lw.new_block(), lw.new_block());
                lw.branch(ok, yes, no);
                lw.switch_to(no);
                let name = lw.any_target_name(cls);
                lw.panic_msg(&format!(
                    "TypeError: the target of this proxy is not a {name}, so its methods cannot be called through the proxy yet (#823)"
                ));
                lw.terminate(Terminator::Unreachable);
                lw.switch_to(yes);
                let data = Operand::Copy(proj(&target, Proj::Field(0)));
                let vt = lw.obj_vtable(data, cls);
                lw.assign(Place::local(out), Rvalue::Use(vt));
            },
            |lw| {
                let vt = lw.obj_vtable(obj.clone(), cls);
                lw.assign(Place::local(out), Rvalue::Use(vt));
            },
        );
        Operand::Copy(Place::local(out))
    }

    /// Before a method call whose receiver `recv` is of a proxiable class: a proxy whose
    /// handler has a `get` trap would have to read the method through it (JavaScript runs
    /// `get(target, "m", proxy)` first), which needs method values as `any`: a `TypeError`
    /// until then rather than a call that skips the trap. Without a `get` trap the method
    /// runs with `this` the proxy, as in JavaScript.
    pub(super) fn proxy_method_check(&mut self, recv: &Operand) {
        if self.dead() {
            return;
        }
        let recv = self.rvalue_temp(Ty::Ptr, Rvalue::Use(recv.clone()));
        self.proxy_split(
            &recv,
            |lw, u| {
                let pty = lw.cx.proxy_class_ty();
                let TyKind::Adt(d, _) = lw.cx.kind(pty) else {
                    super::ice("the `Proxy` class")
                };
                let m = lw
                    .dyn_members_of(d)
                    .into_iter()
                    .find(|m| m.name == "__hasGet" && m.kind == hir::DynMemberKind::Method);
                let Some(m) = m else {
                    super::ice("the `Proxy` class without `__hasGet`")
                };
                let fid = lw.cx.func_for(m.def, vec![]);
                let b = lw.cx.intern(TyKind::Bool);
                let has = lw.finish_call(Callee::Func(fid), vec![u], b, None);
                let (bad, ok) = (lw.new_block(), lw.new_block());
                lw.branch(has, bad, ok);
                lw.switch_to(bad);
                lw.panic_msg("TypeError: calling a method through a proxy whose handler has a `get` trap is not supported yet (#823)");
                lw.terminate(Terminator::Unreachable);
                lw.switch_to(ok);
            },
            |_| {},
        );
    }

    /// In the drop glue of a proxiable type for the value at `place`: a tagged proxy drops
    /// the `Proxy` object (one reference) and goes on to `done`; anything else continues here.
    pub(super) fn proxy_drop_split(&mut self, place: &Place, done: vir::BlockId) {
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let t = self.proxy_tagged(p.clone());
        let (c, h) = (self.new_block(), self.new_block());
        self.branch(t, c, h);
        self.switch_to(c);
        let u = self.proxy_untag(p);
        let pty = self.cx.proxy_class_ty();
        let up = Place::local(self.copy_to_temp(u, Ty::Ptr));
        self.drop_glue(up, pty);
        self.goto(done);
        self.switch_to(h);
    }

    /// In the format glue of the proxiable `ty`: a tagged proxy is printed as the proxy (its
    /// target, as Node prints a proxy); `done` follows.
    pub(super) fn proxy_format_split(
        &mut self,
        buf: &Operand,
        place: &Place,
        depth: &Operand,
        done: vir::BlockId,
    ) {
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let t = self.proxy_tagged(p.clone());
        let (c, h) = (self.new_block(), self.new_block());
        self.branch(t, c, h);
        self.switch_to(c);
        let u = self.proxy_untag(p);
        let pty = self.cx.proxy_class_ty();
        let up = self.copy_to_temp(u, Ty::Ptr);
        let a = self.addr(Place::local(up));
        let f = self.cx.func(Work::Glue(Glue::Format, pty));
        self.call(Callee::Func(f), vec![buf.clone(), a, depth.clone()], None, false);
        self.goto(done);
        self.switch_to(h);
    }

    /// In `field_place` on the proxiable `ty`: a tagged pointer reached by an operation that
    /// does not run traps yet stops with a `TypeError` rather than reading the `Proxy` object.
    pub(super) fn proxy_guard(&mut self, base: &Place) {
        if self.dead() {
            return;
        }
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(base.clone())));
        let t = self.proxy_tagged(p);
        let (c, h) = (self.new_block(), self.new_block());
        self.branch(t, c, h);
        self.switch_to(c);
        self.panic_msg("TypeError: this operation on a proxy is not supported yet (#823)");
        self.terminate(Terminator::Unreachable);
        self.switch_to(h);
    }

    /// `Glue::ProxyGet` of the field type `fty`: `(proxy, key: *string, out: *F)`, the `get`
    /// trap's answer for `key` checked as an `F` into `*out`.
    pub(super) fn proxy_get_glue(&mut self, data: vir::Local, key: vir::Local, out: vir::Local, fty: TyId) {
        let pty = self.cx.proxy_class_ty();
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        self.push_scope(super::ScopeKind::Block);
        let k = self.proxy_key(ProxyKey::Str(key));
        let r = self.proxy_receiver(data, pty);
        let res = Place::local(self.temp(avt));
        if !self.proxy_invoke("__get", data, pty, vec![(k, any), (r, any)], &res) {
            super::ice("the `Proxy` class without `__get`");
        }
        let v = self.any_unbox(&res, any, fty);
        let op = self.deref_param(out, fty);
        self.store(op, v);
        self.scopes.pop();
        self.terminate(Terminator::Return(super::unit()));
    }

    /// `Glue::ProxySet` of the field type `fty`: `(proxy, key: *string, v: *F)` (owns `*v`):
    /// the `set` trap; `false` is Node's `TypeError` (module code is strict).
    pub(super) fn proxy_set_glue(&mut self, data: vir::Local, key: vir::Local, v: vir::Local, fty: TyId) {
        let pty = self.cx.proxy_class_ty();
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        self.push_scope(super::ScopeKind::Block);
        let k = self.proxy_key(ProxyKey::Str(key));
        let shown = self.proxy_key(ProxyKey::Str(key));
        let val = Place::local(self.temp(avt));
        let vp = self.deref_param(v, fty);
        self.any_box_into(Operand::Copy(vp), fty, &val);
        let r = self.proxy_receiver(data, pty);
        let res = Place::local(self.temp(avt));
        let args = vec![(k, any), (val, any), (r, any)];
        if !self.proxy_invoke("__set", data, pty, args, &res) {
            super::ice("the `Proxy` class without `__set`");
        }
        let b = self.cx.intern(TyKind::Bool);
        let ok = self.any_unbox(&res, any, b);
        let (good, bad) = (self.new_block(), self.new_block());
        self.branch(ok, good, bad);
        self.switch_to(bad);
        let ka = Operand::Copy(proj(&shown, Proj::Field(0)));
        self.panic_with_key(
            "TypeError: 'set' on proxy: trap returned falsish for property '",
            &ka,
            "'",
        );
        self.terminate(Terminator::Unreachable);
        self.switch_to(good);
        self.drop_glue(shown, any);
        self.scopes.pop();
        self.terminate(Terminator::Return(super::unit()));
    }
}
