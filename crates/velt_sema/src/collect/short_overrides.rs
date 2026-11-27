//! An override that takes fewer parameters than the method it overrides (#3819), as TypeScript
//! allows: a method that ignores the arguments after its own.
//!
//! A vtable slot has one ABI, the parameters of the method that introduced it, so the shorter
//! method cannot fill the slot it overrides. It gets a slot of its own, which calls through its
//! class or a subclass use and its own overrides fill, and the inherited slot gets an
//! **adapter**: a synthesized method with the base method's parameters that calls the new slot
//! with the first ones. The inherited slot keeps an entry in the class's slot names under a key
//! no method has ([`base_key`]), so a subclass inherits both.

use super::forwarders::{synth_method, Host, Target};
use super::lookup::Found;
use crate::ctx::Ctx;
use crate::defs::{IfaceMethod, RetCheck, RetWant};
use crate::hand_back::hands_back;
use crate::hir::{DefId, PassMode, TyId, TyKind};

/// The slot-name key of the slot (number `slot`) that method key `key` named before a shorter
/// override took the name.
pub(super) fn base_key(key: &str, slot: u32) -> String {
    format!("<base {slot}>{key}")
}

/// How many parameters override `m` (parameter types `mp`) takes when they are the first ones
/// of the base method `b` (`bp`, in the override's terms) and `b` has more. Not for the shapes
/// an adapter does not forward: a rest parameter of the override's own, type parameters of the
/// method's own, a generator or an accessor.
pub(super) fn takes_first(
    cx: &mut Ctx,
    (m, mp): (DefId, &[TyId]),
    (b, bp): (DefId, &[TyId]),
) -> Option<usize> {
    if mp.len() >= bp.len() || !cx.same_tys(mp, &bp[..mp.len()]) {
        return None;
    }
    // A result of `this`: only where both hand back their receiver (`crate::hand_back`), so the
    // adapter returns nothing as every entry of the slot does.
    let this = (cx.fn_info(m).returns_this(), cx.fn_info(b).returns_this());
    let this_ok = match this {
        (false, false) => true,
        (true, true) => hands_back(cx, m) && hands_back(cx, b),
        _ => false,
    };
    // The base method's rest parameter is one of those dropped; the override has none.
    (this_ok && plain(cx, m) && !cx.fn_info(m).rest && plain(cx, b)).then_some(mp.len())
}

/// Is `d` a method an adapter forwards to or stands for ([`takes_first`])?
fn plain(cx: &Ctx, d: DefId) -> bool {
    let f = cx.fn_info(d);
    let owner = (f.owner.and_then(|o| cx.adt(o))).map_or(0, |a| a.generics.len());
    f.generics.len() == owner && !f.is_generator && !f.is_getter
}

/// [`takes_first`] for method `m` (parameter types `mp`, in the implementing type's terms) of a
/// class against the interface method `im` (`want`) it implements.
pub(super) fn implements_first(
    cx: &mut Ctx,
    (m, mp): (DefId, &[TyId]),
    (im, want): (&IfaceMethod, &[TyId]),
) -> Option<usize> {
    if mp.len() >= want.len() || !cx.same_tys(mp, &want[..mp.len()]) {
        return None;
    }
    let class = (cx.fn_info(m).owner.and_then(|o| cx.adt(o))).is_some();
    let plain = plain(cx, m) && !cx.fn_info(m).rest && !cx.fn_info(m).returns_this();
    (class && plain && im.generics.len() == 0 && !im.is_getter).then_some(mp.len())
}

/// The entry of interface method `im` (parameter types `want`, result `ret`) in the
/// implementation by class `d` (its type `self_ty`), whose method `m` takes only the first
/// `take` of its parameters: a method of `d` with the interface method's parameters that calls
/// `m` with the first ones, through `d`'s vtable when `m` has a slot there.
pub(super) fn impl_entry(
    cx: &mut Ctx,
    (d, self_ty): (DefId, TyId),
    (m, owner_args): (DefId, Vec<TyId>),
    (im, want, ret): (&IfaceMethod, &[TyId], TyId),
    take: usize,
) -> DefId {
    let params = (im.params.iter().zip(want))
        .map(|(p, &ty)| {
            let mut p = p.clone();
            p.ty = ty;
            p.default = None;
            p
        })
        .collect();
    let qual = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
    let mutates = (cx.fn_info(m).this.as_ref()).is_some_and(|t| t.mode == PassMode::BorrowMut);
    let target = match cx.vslot(d, &im.name) {
        Some(slot) => Target::SlotFirst { slot, def: m, take },
        None => Target::First {
            def: m,
            targs: owner_args,
            take,
        },
    };
    let host = Host::adt(cx, d);
    let name = format!("{qual}.<dyn {}>", im.name);
    synth_method(cx, &host, self_ty, name, params, ret, mutates, target)
}

/// The adapter of class `d` for its method `m`, which takes the first `take` parameters of the
/// method `base` it overrides and has vtable slot `entry`: a method with `base`'s parameters
/// and result that calls slot `entry` with the first ones.
pub(super) fn adapter(
    cx: &mut Ctx,
    d: DefId,
    m: DefId,
    base: &Found,
    (entry, take): (u32, usize),
) -> DefId {
    let owner_args = base.owner_args();
    let (name, params, ret) = {
        let info = cx.fn_info(base.def());
        let short = info.name.rsplit('.').next().unwrap_or("").to_string();
        (short, info.params.clone(), info.ret)
    };
    let params = (params.into_iter())
        .map(|mut p| {
            p.ty = cx.subst(p.ty, &owner_args);
            p.default = None;
            p
        })
        .collect();
    // A result the base method takes from its body is not known yet: given once it is.
    let pending = super::ret_infer::is_pending(cx, base.def());
    let ret = match (pending, hands_back(cx, base.def())) {
        // Every entry of a slot of methods that hand back their receiver returns nothing.
        (_, true) => cx.ty.unit,
        (true, false) => cx.ty.error,
        (false, false) => cx.subst(ret, &owner_args),
    };
    let pending = pending && !hands_back(cx, base.def());
    let n = cx.adt(d).map_or(0, |a| a.generics.len());
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let self_ty = cx.ty.intern(TyKind::Adt(d, self_args));
    let qual = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
    let mutates = (cx.fn_info(m).this.as_ref()).is_some_and(|t| t.mode == PassMode::BorrowMut);
    let host = Host::adt(cx, d);
    let def = synth_method(
        cx,
        &host,
        self_ty,
        format!("{qual}.<base {name}>"),
        params,
        ret,
        mutates,
        Target::SlotFirst {
            slot: entry,
            def: m,
            take,
        },
    );
    if pending {
        cx.ret_checks.push(RetCheck {
            def,
            args: vec![],
            want: RetWant::Given(base.def(), owner_args),
            span: host.span,
            message: String::new(),
        });
    }
    def
}
