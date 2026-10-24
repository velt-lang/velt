//! `WeakMap`, `WeakSet` and `WeakRef` (std/prelude/weak.vlt) on velt_rt's weak-reference core
//! (rt_abi.md "Weak references"; docs/internals/design/weak-refs.md).
//!
//! Keys and `WeakRef` targets are counted objects passed by address (the map does not count
//! them). A map value is one `u64` word ([`WordKind`]): the bits of a small plain value, a
//! counted object pointer (null for `null`), or the pointer of a counted box holding any other
//! value (strings, unions, tuples, function values). The glue a map is created with (on its
//! first `set`) retains, releases and traces those words (glue/trace.rs).

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use super::glue::Glue;
use super::operand::proj;
use super::rt::Rt;
use super::{cfunc, cint, ice, FnLower, Work};
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

/// How a weak map stores a value of some type in its `u64` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::lower) enum WordKind {
    /// The value's bits (no drop, at most 8 bytes): numbers, `bool`, C-like enums.
    Plain,
    /// A pointer to a counted object of the given type, or null (`T | null` of one).
    Direct(TyId),
    /// A counted box holding the value.
    Boxed,
}

/// The prelude's weak classes, by qualified name.
const WEAK_CLASSES: [&str; 3] = [
    "std/prelude/weak::WeakMap",
    "std/prelude/weak::WeakSet",
    "std/prelude/weak::WeakRef",
];

impl super::Cx<'_> {
    /// The prelude's `WeakMap`, `WeakSet` or `WeakRef` class (by name, for messages), if `t`
    /// is one of them or a subclass of one (`class Cache extends WeakMap<K, V>`: the same
    /// runtime handle, with fields of its own).
    pub(in crate::lower) fn weak_class(&self, t: TyId) -> Option<&'static str> {
        let TyKind::Adt(d, _) = self.types.kind(t) else {
            return None;
        };
        if !self.is_class(t) {
            return None;
        }
        let root = &self.adt_def(self.class_root(*d)).name;
        WEAK_CLASSES
            .iter()
            .find(|n| root == **n)
            .map(|n| n.rsplit("::").next().unwrap_or(n))
    }
}

impl FnLower<'_, '_> {
    /// How weak maps store values of `ty`.
    pub(in crate::lower) fn word_kind(&mut self, ty: TyId) -> WordKind {
        let vt = self.cx.ty(ty);
        if !self.cx.needs_drop(ty) && self.cx.size_align(vt).0 <= 8 {
            return WordKind::Plain;
        }
        match self.cx.kind(ty) {
            // A typed proxy's word has bit 0 set, which the runtime must not see as an object.
            _ if self.cx.proxiable(ty) => WordKind::Boxed,
            TyKind::Option(e, _) if self.cx.proxiable(e) => WordKind::Boxed,
            _ if self.cx.counted(ty) => WordKind::Direct(ty),
            TyKind::Option(e, _) if vt == Ty::Ptr && self.cx.counted(e) => WordKind::Direct(e),
            _ => WordKind::Boxed,
        }
    }

    pub(super) fn weak_intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr], ty: TyId) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::WeakMapSet, [m, key, value]) => self.weakmap_set(m, key, value),
            (I::WeakMapGet | I::WeakMapHas | I::WeakMapDelete, [m, key]) => {
                let m = self.expr(m);
                let k = match self.sub(key.ty) {
                    kt if self.dyn_weak_key(kt) => {
                        self.dyn_key(key, kt).map(|k| self.dyn_probe_addr(&k))
                    }
                    // A view is keyed by the object it views (weak_view.rs).
                    kt if self.view_weak_key(kt) => {
                        self.view_key(key, kt).map(|k| self.dyn_probe_addr(&k))
                    }
                    _ => self.weak_key(key, true),
                };
                let Some(k) = k else {
                    return match i {
                        I::WeakMapGet => cint(0, Ty::U64),
                        _ => Operand::Const(crate::vir::Const::Bool(false), Ty::Bool),
                    };
                };
                match i {
                    I::WeakMapGet => {
                        let found = self.temp(Ty::U8);
                        let fa = self.addr(Place::local(found));
                        let d = self.temp(Ty::U64);
                        self.call_rt(Rt::WeakmapGet, vec![m, k, fa], Some(Place::local(d)));
                        Operand::Copy(Place::local(d))
                    }
                    I::WeakMapHas => self.rt_u8(Rt::WeakmapHas, vec![m, k]),
                    _ => self.rt_u8(Rt::WeakmapDelete, vec![m, k]),
                }
            }
            (I::WeakMapValue, [w]) => {
                let w = self.expr(w);
                let t = self.sub(ty);
                let v = self.word_value(w, t);
                self.own_value(v, t)
            }
            (I::WeakRefNew, [target]) => {
                let kt = self.sub(target.ty);
                if self.dyn_weak_key(kt) {
                    return self.weakref_new_dyn(target, kt);
                }
                if self.view_weak_key(kt) {
                    return self.weakref_new_view(target, kt);
                }
                let Some(k) = self.weak_key(target, false) else {
                    return cint(0, Ty::U32);
                };
                let d = self.temp(Ty::U32);
                self.call_rt(Rt::WeakrefNew, vec![k], Some(Place::local(d)));
                Operand::Copy(Place::local(d))
            }
            (I::WeakRefDeref, [r]) => {
                let r = self.expr(r);
                let t = self.sub(ty);
                let obj = match self.cx.kind(t) {
                    TyKind::Option(e, _) => e,
                    _ => t,
                };
                if self.view_weak_key(obj) {
                    return self.weakref_deref_view(r, t);
                }
                if self.dyn_weak_key(obj) {
                    self.cx.note_weak_object();
                    if !self.cx.weak_object() {
                        self.cx.facts.unmet = true;
                        let vt = self.cx.ty(t);
                        return self.zero_value(vt);
                    }
                    return self.weakref_deref_dyn(r, t, obj);
                }
                if let TyKind::Option(e, _) = self.cx.kind(t) {
                    self.cx.note_weak_seed(e);
                }
                let d = self.temp(Ty::Ptr);
                self.call_rt(Rt::WeakrefDeref, vec![r], Some(Place::local(d)));
                if self.cx.ty(t) != Ty::Ptr {
                    // The target is counted in the next pass.
                    self.cx.facts.unmet = true;
                    return self.none_value(t);
                }
                self.own_value(Operand::Copy(Place::local(d)), t)
            }
            _ => ice(format_args!(
                "intrinsic {i:?} called with {} arguments",
                args.len()
            )),
        }
    }

    /// The address of the object `key` (borrowed), whose type becomes counted and weak-capable;
    /// `None` in a pass where it is not counted yet. A typed proxy's is its `Proxy` object's,
    /// with the tag cleared when `untag` (a map key: the identity is the proxy's, as for an
    /// `any` holding it); a `WeakRef` keeps the tag (velt_rt gives it back from `deref`).
    fn weak_key(&mut self, key: &hir::Expr, untag: bool) -> Option<Operand> {
        let kt = self.sub(key.ty);
        self.cx.note_identity(kt);
        self.cx.note_share(kt);
        self.cx.note_weak_seed(kt);
        let k = self.expr(key);
        if self.cx.counted(kt) && self.cx.ty(kt) == Ty::Ptr {
            if !self.cx.proxiable(kt) {
                return Some(k);
            }
            // The `Proxy` object may be weakly held too, so it releases as weak-capable.
            let pty = self.cx.proxy_class_ty();
            self.cx.note_weak_seed(pty);
            return Some(match untag {
                true => self.proxy_untag(k),
                false => self.rvalue_temp(Ty::Ptr, Rvalue::Use(k)),
            });
        }
        let countable = self.cx.is_class(kt)
            || self.cx.copied_object(kt)
            || matches!(self.cx.kind(kt), TyKind::Array(_));
        if countable {
            // Counted in the next pass.
            self.cx.facts.unmet = true;
        } else {
            // Sema rejects such keys where it sees them; a generic one instantiated with a
            // value that is not an object gets here.
            let name = self.cx.type_name(kt);
            self.panic_msg(&format!(
                "TypeError: Invalid value used as weak map key (`{name}` is not an object)"
            ));
        }
        None
    }

    /// `__intrinsic_weakmap_set(m, key, value)`: the map (created now when `m` is 0).
    fn weakmap_set(&mut self, m: &hir::Expr, key: &hir::Expr, value: &hir::Expr) -> Operand {
        let m = self.expr(m);
        let kt = self.sub(key.ty);
        // An `object` key (issue #926, weak_object.rs): checked, with its own trace glue.
        let dk = match self.dyn_weak_key(kt) {
            true => Some(self.dyn_key(key, kt)),
            // A view is keyed by the object it views (weak_view.rs).
            false if self.view_weak_key(kt) => Some(self.view_key(key, kt)),
            false => None,
        };
        let (k, dyn_key) = match dk {
            Some(Some(d)) => (Some(d.data.clone()), Some(d)),
            Some(None) => (None, None),
            None => (self.weak_key(key, true), None),
        };
        let vty = self.sub(value.ty);
        self.cx.note_share(vty);
        self.cx.note_weak_seed(vty);
        let v = self.consume(value);
        let Some(k) = k else {
            return m;
        };
        if let Some(d) = &dyn_key {
            // `WeakSet.add` stores `true` for each member.
            let set = matches!(value.kind, hir::ExprKind::Lit(hir::Lit::Bool(true)));
            let what = match set {
                true => super::weak_object::WeakUse::SetAdd,
                false => super::weak_object::WeakUse::MapSet,
            };
            self.dyn_key_check(d, what);
        }
        let out = self.temp(Ty::U32);
        self.assign(Place::local(out), Rvalue::Use(m.clone()));
        let (new_bb, join) = (self.new_block(), self.new_block());
        let none = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(crate::vir::BinOp::Eq, m, cint(0, Ty::U32)),
        );
        self.branch(none, new_bb, join);
        self.switch_to(new_bb);
        // A key that may be a `Proxy` object is not a `kt` (and an `object` key brings its
        // own): none for the map (at worst a leak; velt_rt uses each key's record).
        let key_trace = match dyn_key.is_some() || self.cx.proxiable(kt) {
            true => cint(0, Ty::Ptr),
            false => self.trace_fn(kt),
        };
        let null = cint(0, Ty::Ptr);
        let (retain, release, trace) = match self.word_kind(vty) {
            WordKind::Plain => (null.clone(), null.clone(), null),
            WordKind::Direct(c) => {
                let r = cfunc(self.cx.func(Work::Glue(Glue::WeakRetain, vty)));
                let d = cfunc(self.cx.func(Work::Glue(Glue::WeakRelease, vty)));
                (r, d, self.trace_fn(c))
            }
            WordKind::Boxed => {
                let r = cfunc(self.cx.func(Work::Glue(Glue::WeakRetain, vty)));
                let d = cfunc(self.cx.func(Work::Glue(Glue::WeakRelease, vty)));
                let t = match self.cx.weak_refs_inline(None, vty).is_empty() {
                    true => null,
                    false => cfunc(self.cx.func(Work::Glue(Glue::WeakBoxTrace, vty))),
                };
                (r, d, t)
            }
        };
        self.call_rt(
            Rt::WeakmapNew,
            vec![key_trace, retain, release, trace],
            Some(Place::local(out)),
        );
        self.goto(join);
        self.switch_to(join);
        let w = self.value_word(v, vty);
        let mo = Operand::Copy(Place::local(out));
        match &dyn_key {
            Some(d) => {
                let tr = self.dyn_key_trace(d);
                self.call_rt(Rt::WeakmapSetTraced, vec![mo.clone(), k, w, tr], None);
            }
            None => self.call_rt(Rt::WeakmapSet, vec![mo.clone(), k, w], None),
        }
        mo
    }

    /// The word of the owned `ty` value `v` (its ownership moves into the word).
    fn value_word(&mut self, v: Operand, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        match self.word_kind(ty) {
            WordKind::Plain => {
                let w = self.temp(Ty::U64);
                self.assign(Place::local(w), Rvalue::Use(cint(0, Ty::U64)));
                if vt != Ty::Unit {
                    let a = self.addr(Place::local(w));
                    let ap = self.operand_place(a, Ty::Ptr);
                    self.assign(proj(&ap, Proj::Deref(vt)), Rvalue::Use(v));
                }
                Operand::Copy(Place::local(w))
            }
            WordKind::Direct(_) => self.cast_to(v, Ty::Ptr, Ty::U64),
            WordKind::Boxed => {
                let p = self.counted_alloc(vt);
                let pp = self.operand_place(p.clone(), Ty::Ptr);
                self.assign(proj(&pp, Proj::Deref(vt)), Rvalue::Use(v));
                self.cast_to(p, Ty::Ptr, Ty::U64)
            }
        }
    }

    /// The `ty` value of the word `w`, taking over the reference the word holds.
    fn word_value(&mut self, w: Operand, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        match self.word_kind(ty) {
            WordKind::Plain => {
                if vt == Ty::Unit {
                    return super::unit();
                }
                let t = self.temp(Ty::U64);
                self.assign(Place::local(t), Rvalue::Use(w));
                let a = self.addr(Place::local(t));
                let ap = self.operand_place(a, Ty::Ptr);
                self.rvalue_temp(vt, Rvalue::Use(Operand::Copy(proj(&ap, Proj::Deref(vt)))))
            }
            WordKind::Direct(_) => self.cast_to(w, Ty::U64, Ty::Ptr),
            WordKind::Boxed => {
                // A copy sharing the boxed value's parts (as `Map.get` returns a value), then
                // the box loses the reference `get` gave.
                let p = self.cast_to(w, Ty::U64, Ty::Ptr);
                let pp = self.operand_place(p.clone(), Ty::Ptr);
                let held =
                    self.rvalue_temp(vt, Rvalue::Use(Operand::Copy(proj(&pp, Proj::Deref(vt)))));
                let v = self.share_value(held, ty);
                self.release_weak_box(p, ty);
                v
            }
        }
    }
}
