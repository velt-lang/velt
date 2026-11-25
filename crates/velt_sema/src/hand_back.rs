//! Methods that hand back their receiver (#3738). A method whose result is the polymorphic
//! `this` (`add(x: T): this`, or an override of one) can only return its receiver
//! (`body::expr::this_type`), so it is compiled to return nothing, and each call gives back the
//! receiver it was called on (`body::expr::hand_back`): a call whose result is unused
//! (`s.add(x);`) shares nothing, a chain (`s.add(1).add(2)`) reads the receiver again, and only
//! a kept result (`const t = s.add(3)`) is another reference to it. Returning `this` itself
//! would share the receiver at every call, which makes every object of the class reference
//! counted in the whole program (as returning the array would for `sort`, std/prelude/array.velt).
//!
//! Tables that call a method for its result (an interface's implementation, a vtable slot's
//! trampoline, the methods `any` calls by name) take a [`value_entry`] instead: a method of the
//! class that calls it and returns `this`, which is a share only where such a table is used.

use crate::collect::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::hir::{
    self, AdtKind, Callee, Def, DefId, ExprKind as H, LocalId, PassMode, StmtKind as S, TyId,
    TyKind, UseMode,
};

/// Does method `def` hand back its receiver? A class method returning `this` (not a getter, an
/// async method, a generator, a formatting hook (`crate::hooks`) or a method with type
/// parameters of its own) whose overridden methods all return `this` too, so that every entry
/// of its vtable slot returns nothing alike.
pub(crate) fn hands_back(cx: &Ctx, def: DefId) -> bool {
    let f = cx.fn_info(def);
    if !f.returns_this() || f.is_getter || f.is_async || f.is_async_gen || f.is_generator {
        return false;
    }
    let Some(owner) = f.owner else {
        return false;
    };
    let Some(a) = cx.adt(owner).filter(|a| a.kind == AdtKind::Class) else {
        return false;
    };
    if f.generics.len() != a.generics.len() {
        return false;
    }
    let Some(key) = (a.methods.iter())
        .find(|(_, m)| m.def == def && !m.is_static)
        .map(|(k, _)| k.clone())
    else {
        return false;
    };
    if crate::hooks::NAMES.contains(&key.as_str()) {
        return false;
    }
    let mut base = a.base;
    while let Some((b, _)) = base.and_then(|t| cx.class_of(t)) {
        let Some(ba) = cx.adt(b) else {
            break;
        };
        let overridden = ba.methods.get(&key).filter(|m| !m.is_static);
        if overridden.is_some_and(|m| !cx.fn_info(m.def).returns_this()) {
            return false;
        }
        base = ba.base;
    }
    true
}

/// The method a table calls for `def`'s result: `def` itself, or for a method that hands back
/// its receiver, a method of its class (`C.<value m>`) that calls it with the same parameters
/// and modes and returns `this`.
pub(crate) fn value_entry(cx: &mut Ctx, def: DefId) -> DefId {
    if !hands_back(cx, def) {
        return def;
    }
    let owner = cx
        .fn_info(def)
        .owner
        .expect("ICE: a hand-back method has a class");
    let host = Host::adt(cx, owner);
    let args: Vec<TyId> = (0..host.generics.len() as u32)
        .map(|i| cx.ty.param(i))
        .collect();
    let self_ty = cx.ty.intern(TyKind::Adt(owner, args.clone()));
    let f = cx.fn_info(def);
    let qual = cx
        .adt(owner)
        .map(|a| a.qual_name.clone())
        .unwrap_or_default();
    let short = f.name.rsplit('.').next().unwrap_or_default();
    let name = format!("{qual}.<value {short}>");
    let mut params = f.params.clone();
    for p in &mut params {
        p.default = None;
    }
    let (ret, this_mode) = (f.ret, f.this.as_ref().map(|t| t.mode));
    let mut_this = this_mode == Some(PassMode::BorrowMut);
    let target = Target::Default(def, args);
    let entry = synth_method(cx, &host, self_ty, name, params, ret, mut_this, target);
    return_receiver(cx, entry);
    entry
}

/// Makes synthesized method `entry`, whose body is `return callee(this, p1, …);` with a callee
/// that hands back its receiver, `callee(this, p1, …); return this;`: the call returns nothing
/// and the entry returns `this` (as the declared result's class). Each argument is passed as the callee
/// takes it (synthesized after the callee's modes are inferred, `any::dyn_members`).
pub(crate) fn return_receiver(cx: &mut Ctx, entry: DefId) {
    let unit = cx.ty.unit;
    let callee_modes = |cx: &Ctx, callee: &Callee| match callee {
        Callee::Def(d, _) => Some(cx.fn_info(*d).params.iter().map(|p| p.mode).collect()),
        _ => None,
    };
    let Some(Def::Fn(mut f)) = cx.defs[entry.0 as usize].take() else {
        panic!("ICE: return_receiver on a synthesized method");
    };
    let self_ty = f.params[0].ty;
    let span = f.span;
    let mut call = match f.body.block.stmts.pop().map(|s| s.kind) {
        Some(S::Return(Some(call)) | S::Expr(call)) => call,
        _ => panic!("ICE: a synthesized method ends in its call"),
    };
    call.ty = unit;
    if let H::Call { callee, args } = &mut call.kind {
        let modes: Option<Vec<PassMode>> = callee_modes(cx, callee);
        for (k, mode) in modes.into_iter().flatten().enumerate() {
            if let Some(p) = f.params.get_mut(k + 1) {
                p.mode = mode;
            }
            if let (Some(a), Some(m)) = (args.get_mut(k + 1), use_mode(mode)) {
                if let H::Local(_, use_) = &mut a.kind {
                    *use_ = m;
                }
            }
        }
    }
    let modes: Vec<PassMode> = f.params[1..].iter().map(|p| p.mode).collect();
    for (p, mode) in cx.fn_info_mut(entry).params.iter_mut().zip(modes) {
        p.mode = mode;
    }
    // The borrowed `this` returned is shared where it is returned, as for any method that
    // returns `this` (so a table that calls this entry is what counts the class).
    let mut value = hir::Expr {
        kind: H::Local(LocalId(0), UseMode::Borrow),
        ty: self_ty,
        span,
    };
    if f.ret != self_ty {
        value = hir::Expr {
            kind: H::Upcast(Box::new(value)),
            ty: f.ret,
            span,
        };
    }
    f.body.block.stmts.extend([
        hir::Stmt {
            kind: S::Expr(call),
            span,
        },
        hir::Stmt {
            kind: S::Return(Some(value)),
            span,
        },
    ]);
    cx.defs[entry.0 as usize] = Some(Def::Fn(f));
}

/// How an argument is read for a parameter passed `mode` (`None`: as it is, a `Copy`).
fn use_mode(mode: PassMode) -> Option<UseMode> {
    match mode {
        PassMode::Copy => None,
        PassMode::Borrow => Some(UseMode::Borrow),
        PassMode::BorrowMut => Some(UseMode::BorrowMut),
        PassMode::Owned => Some(UseMode::Move),
    }
}
