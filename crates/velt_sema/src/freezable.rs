//! Values frozen at run time (#2623): `Object.freeze(x)` of a value made before, whose type the
//! program writes, or whose values reach `Object.isFrozen`. Node throws a `TypeError` on a write
//! to a frozen object's field, so such a type keeps a mark on each object (`RC_FROZEN` in its
//! count word, rt_abi.md "Counted objects"): `Intrinsic::Freeze` sets it, `Intrinsic::IsFrozen`
//! reads it, and every field write of the type tests it first and calls std's
//! `frozenWriteError`, an ordinary throw (`try` catches it, throws inference sees it).
//!
//! Which types need the mark is known once every body is checked (`object_copies::check`
//! gathers the writes and the `Object.freeze` sites), so a program that finds such types is
//! checked again with them known, as for the other facts a round learns too late
//! (`crate::rounds`): their keys ride in `lazy_std::Assumed::frozen`, and in that round
//! [`Ctx::frozen_checked`] makes each write of one tested and `Object.freeze` sets the mark. The
//! set is closed over the types sharing a value's storage (`object_copies::related`), so a
//! write through a view type is tested too (`hir::Program::freezable`).
//!
//! A type is freezable when it is an object type without type parameters (not a class, nor an
//! interface with methods, whose objects' own writes aren't followed, nor a generic object
//! type: those keep the "not supported yet" error).

use rustc_hash::FxHashSet;

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};

impl Ctx<'_> {
    /// The key of object type `d` that stays the same from round to round (a `DefId` of an
    /// anonymous type may not): the type as shown, over its type parameters. Two types shown
    /// the same way share a key, so both are freezable: that only tests more writes.
    pub(crate) fn frozen_key(&mut self, d: DefId) -> Option<String> {
        let n = self.adt(d)?.generics.names.len();
        let args: Vec<TyId> = (0..n as u32).map(|i| self.ty.param(i)).collect();
        let t = self.ty.intern(TyKind::Adt(d, args));
        Some(self.display(t))
    }

    /// Is `t` (an object type, or the payload of `T | null` of one) one this round tests the
    /// writes of: a freezable type (module docs)?
    pub(crate) fn frozen_checked(&mut self, t: TyId) -> bool {
        if self.objects.frozen_keys.is_empty() {
            return false;
        }
        let t = self.ty.opt_payload(t).unwrap_or(t);
        let TyKind::Adt(d, _) = *self.ty.kind(t) else {
            return false;
        };
        if !self.is_object_type(t) {
            return false;
        }
        let Some(key) = self.frozen_key(d) else {
            return false;
        };
        self.objects.frozen_keys.contains(&key)
    }

    /// Can values of `t` carry a frozen mark: an object type (module docs)?
    pub(crate) fn may_be_freezable(&mut self, t: TyId) -> bool {
        let t = self.ty.opt_payload(t).unwrap_or(t);
        matches!(self.ty.kind(t), TyKind::Adt(_, args) if args.is_empty()) && self.is_object_type(t)
    }

    /// Record object type `t` (with the types sharing its storage) as freezable, for the next
    /// round's check (module docs).
    pub(crate) fn found_freezable(&mut self, t: TyId) {
        let t = self.ty.opt_payload(t).unwrap_or(t);
        let TyKind::Adt(d, _) = *self.ty.kind(t) else {
            return;
        };
        if let Some(key) = self.frozen_key(d) {
            self.objects.frozen_found.insert(key);
        }
    }
}

/// Close the freezable types found this round over the object types sharing their storage
/// (`object_copies::related`), for the next round (module docs).
pub(crate) fn close_over_views(cx: &mut Ctx, found: &[TyId]) {
    if found.is_empty() {
        return;
    }
    let defs: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|&d| cx.adt(d).is_some_and(|a| a.generics.names.is_empty()))
        .collect();
    let mut objects = vec![];
    for d in defs {
        let t = cx.ty.intern(TyKind::Adt(d, vec![]));
        if cx.is_object_type(t) {
            objects.push(t);
        }
    }
    for &f in found {
        cx.found_freezable(f);
        for &o in &objects {
            if o != f && crate::object_copies::related(cx, o, f) {
                cx.found_freezable(o);
            }
        }
    }
}

/// The program's freezable types (`hir::Program::freezable`): the object types whose keys this
/// round assumed.
pub(crate) fn finish(cx: &mut Ctx) {
    if cx.objects.frozen_keys.is_empty() {
        return;
    }
    for i in 0..cx.info.len() as u32 {
        let d = DefId(i);
        let Some(n) = cx.adt(d).map(|a| a.generics.names.len()) else {
            continue;
        };
        let args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
        let t = cx.ty.intern(TyKind::Adt(d, args));
        if n == 0
            && cx.is_object_type(t)
            && cx
                .frozen_key(d)
                .is_some_and(|k| cx.objects.frozen_keys.contains(&k))
        {
            cx.objects.freezable.insert(d);
            // Each value has a count word for the mark (hir_encodings.md "Sharing").
            if !cx.counted_objects.contains(&t) {
                cx.counted_objects.push(t);
            }
        }
    }
}

/// Did this round find freezable types the round didn't know (`Assumed::frozen`)? Then they
/// are assumed from the next round on, which is checked again (`crate::rounds`).
pub(crate) fn grown(cx: &mut Ctx, assumed: &mut Vec<String>) -> bool {
    let found = std::mem::take(&mut cx.objects.frozen_found);
    let new: Vec<String> = found.into_iter().filter(|k| !assumed.contains(k)).collect();
    if new.is_empty() {
        return false;
    }
    assumed.extend(new);
    assumed.sort();
    true
}

/// Start a round with the freezable types the rounds before found (`Assumed::frozen`).
pub(crate) fn assume(cx: &mut Ctx, assumed: &[String]) {
    cx.objects.frozen_keys = assumed.iter().cloned().collect::<FxHashSet<_>>();
}
