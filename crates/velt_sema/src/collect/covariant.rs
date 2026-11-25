//! Covariant results of interface implementations (#3771). A class method implements an
//! interface method whose result its own converts to as a call converts it: a class that
//! declares `implements` of the result interface (`step(): Counter` for `step(): Fluent`), an
//! interface that has the result interface's members (`step(): Sub` with `Sub extends
//! Fluent`), a subclass of the result class, the same with `| null` on both sides, or `this`,
//! as the implementing class (also when a base class declares the method). TypeScript also
//! accepts a class that matches the interface without declaring it; Velt converts a class to
//! an interface only through `implements`, at a call as here.
//!
//! The implementation's table entry is then a method of the class, `C.<Fluent.step>`, that
//! calls the method's own entry (the method, its vtable slot's trampoline, or the entry that
//! returns the receiver of a method returning `this`, `crate::hand_back`) and converts the
//! result: an `Upcast` to a base class or interface, or a `ToDyn` through the result class's
//! implementation of the interface. Its implementation index is known once the batch's
//! implementations are built (`finish`). A method with type parameters of its own gives the
//! entry the same ones, and the entry passes them on. Calls through the class itself still
//! call the method directly.

use velt_common::{Diagnostic, Span};

use super::forwarders::{synth_method, Host, Target};
use super::ret_infer::is_pending;
use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::defs::{Generics, ParamSig};
use crate::hir::{
    self, Def, DefId, ExprKind as H, LocalId, PatKind as P, StmtKind as S, TyId, TyKind, UseMode,
};

/// How the result of an implementing method converts to the interface method's.
#[derive(Clone, Copy, Debug)]
pub(super) enum Conv {
    /// To a base class or an interface value's wider interface (`want`), or the same with
    /// `| null` on both sides.
    Upcast(TyId),
    /// To interface `iface` (as `want`), through the result class's implementation of it.
    ToDyn { iface: DefId, want: TyId },
    /// `have | null` to `want | null` (`whole`) where `have` converts to `want` by a `ToDyn`
    /// through the implementation of `iface`, or by an interface upcast (`iface: None`): tested
    /// for null, as a call converts it (`Counter | null` for `Fluent | null`).
    Payload {
        iface: Option<DefId>,
        have: TyId,
        want: TyId,
        whole: TyId,
    },
}

impl Conv {
    /// The type converted to.
    fn want(&self) -> TyId {
        match *self {
            Conv::Upcast(w) | Conv::ToDyn { want: w, .. } | Conv::Payload { whole: w, .. } => w,
        }
    }
}

/// A converting entry whose `ToDyn` waits for its implementation index (`finish`): the entry,
/// the class type it converts and the interface.
pub(super) type Pending = (DefId, TyId, DefId);

/// The implementing method: `def`, declared by a class instantiated with `owner_args`, found
/// on the class whose self type is `self_ty`.
#[derive(Clone, Copy)]
pub(super) struct Imp<'a> {
    pub def: DefId,
    pub owner_args: &'a [TyId],
    pub self_ty: TyId,
}

impl Imp<'_> {
    /// The declaring class's arguments followed by the method's own type parameters, as the
    /// implementing method sees them (`Param(owner_args.len() + k)`).
    fn full_args(&self, cx: &mut Ctx) -> Vec<TyId> {
        let n = self.owner_args.len();
        let own = cx.fn_info(self.def).generics.len().saturating_sub(n);
        let mut args = self.owner_args.to_vec();
        args.extend((0..own).map(|k| cx.ty.param((n + k) as u32)));
        args
    }

    /// The method's declared result, as the implementing method sees it.
    fn declared(&self, cx: &mut Ctx) -> TyId {
        let args = self.full_args(cx);
        let ret = cx.fn_info(self.def).ret;
        cx.subst(ret, &args)
    }

    /// The method's result where it is implemented: a `this` result is the implementing class.
    fn result(&self, cx: &mut Ctx) -> TyId {
        match cx.fn_info(self.def).returns_this() {
            true => self.self_ty,
            false => self.declared(cx),
        }
    }
}

/// How the method's result, which differs from `want`, converts to it, if it does as a call
/// would. `None` for an equal result, and while the result is still being inferred.
pub(super) fn conversion(cx: &mut Ctx, imp: Imp, want: TyId) -> Option<Conv> {
    if is_pending(cx, imp.def) || !own_generics_fit(cx, imp) {
        return None;
    }
    // A declared result equal to the interface's needs no entry (an inherited `this` too).
    let declared = imp.declared(cx);
    let have = imp.result(cx);
    if cx.same_ty(declared, want) || cx.same_ty(have, want) {
        return None;
    }
    converts(cx, have, want)
}

/// How `have` converts to `want` (`conversion`).
fn converts(cx: &mut Ctx, have: TyId, want: TyId) -> Option<Conv> {
    match (cx.ty.kind(have).clone(), cx.ty.kind(want).clone()) {
        (TyKind::Dyn(from, fargs), TyKind::Dyn(to, targs))
            if cx.dynamic(have).is_none()
                && cx.dynamic(want).is_none()
                && !cx.is_generic_fn(want)
                && cx.iface_upcast((from, &fargs), (to, &targs)) =>
        {
            cx.generic_called.upcasts.insert((from, to));
            Some(Conv::Upcast(want))
        }
        (_, TyKind::Dyn(iface, args)) if implements(cx, have, iface, &args) => {
            Some(Conv::ToDyn { iface, want })
        }
        (_, TyKind::Adt(..)) if cx.class_of(want).is_some() && is_base(cx, have, want) => {
            Some(Conv::Upcast(want))
        }
        // `Square | null` for `Shape | null`: the same pointer or null (a call converts it
        // by testing for null; the entry needs no test).
        (TyKind::Option(x, m), TyKind::Option(y, n))
            if m == n && cx.class_of(y).is_some() && is_base(cx, x, y) =>
        {
            Some(Conv::Upcast(want))
        }
        // `Counter | null` for `Fluent | null`: the payload converted when it is there.
        (TyKind::Option(x, m), TyKind::Option(y, n)) if m == n => {
            let iface = match converts(cx, x, y)? {
                Conv::ToDyn { iface, .. } => Some(iface),
                Conv::Upcast(_) if matches!(cx.ty.kind(y), TyKind::Dyn(..)) => None,
                _ => return None,
            };
            Some(Conv::Payload {
                iface,
                have: x,
                want: y,
                whole: want,
            })
        }
        _ => None,
    }
}

/// A method with type parameters of its own declared by a class with as many as the
/// implementing class: the entry takes the class's and then the method's (a method inherited
/// from a base class with other type parameters is not converted).
fn own_generics_fit(cx: &mut Ctx, imp: Imp) -> bool {
    let n = imp.owner_args.len();
    if cx.fn_info(imp.def).generics.len() <= n {
        return true;
    }
    cx.class_of(imp.self_ty)
        .is_some_and(|(_, args)| args.len() == n)
}

/// Does class type `t` (or a base class of it) declare `implements iface<args>`?
fn implements(cx: &mut Ctx, t: TyId, iface: DefId, args: &[TyId]) -> bool {
    let mut cur = Some(t);
    for _ in 0..64 {
        let Some((d, targs)) = cur.and_then(|t| cx.class_of(t)) else {
            return false;
        };
        let bounds = cx.adt(d).map(|a| a.implements.clone()).unwrap_or_default();
        for b in bounds.iter().filter(|b| b.iface == iface) {
            let bargs: Vec<TyId> = b.args.iter().map(|a| cx.subst(*a, &targs)).collect();
            if cx.same_tys(&bargs, args) {
                return true;
            }
        }
        cur = cur.and_then(|t| cx.base_of(t));
    }
    false
}

/// Is class type `base` a base class of class type `t`?
fn is_base(cx: &mut Ctx, t: TyId, base: TyId) -> bool {
    let mut cur = cx.base_of(t);
    for _ in 0..64 {
        let Some(b) = cur else {
            return false;
        };
        if cx.same_ty(b, base) {
            return true;
        }
        cur = cx.base_of(b);
    }
    false
}

/// The table entry of class `d` for interface method `name` of `iname`, implemented by `imp`
/// through `entry`, whose result converts by `conv`: a method calling `entry` and converting
/// what it returns.
pub(super) fn entry(
    cx: &mut Ctx,
    (d, imp): (DefId, Imp),
    (iname, name): (&str, &str),
    entry: DefId,
    conv: Conv,
    pending: &mut Vec<Pending>,
) -> DefId {
    let mut host = Host::adt(cx, d);
    let n = host.generics.len();
    let full_args = imp.full_args(cx);
    let own = &full_args[imp.owner_args.len()..];
    // The method's own type parameters follow the class's in the entry too.
    if !own.is_empty() {
        let g = cx.fn_info(imp.def).generics.clone();
        host.generics = with_own(&host.generics, &g, imp.owner_args.len());
    }
    // The entry is a method of `d` (a trampoline) or of the class declaring the method.
    let mut targs: Vec<TyId> = match cx.fn_info(entry).owner == Some(d) {
        true => (0..n as u32).map(|i| cx.ty.param(i)).collect(),
        false => imp.owner_args.to_vec(),
    };
    targs.extend_from_slice(own);
    let params: Vec<ParamSig> = (cx.fn_info(imp.def).params.clone().into_iter())
        .map(|mut p| {
            p.ty = cx.subst(p.ty, &full_args);
            p.default = None;
            p
        })
        .collect();
    let have = imp.result(cx);
    let declared = imp.declared(cx);
    let want = conv.want();
    let mut_this = cx
        .fn_info(imp.def)
        .this
        .as_ref()
        .is_some_and(|t| t.mode == hir::PassMode::BorrowMut);
    let qual = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
    let label = format!("{qual}.<{iname}.{name}>");
    let target = Target::Default(entry, targs);
    let self_ty = imp.self_ty;
    let e = synth_method(cx, &host, self_ty, label, params, want, mut_this, target);
    let downcast = !cx.same_ty(declared, have);
    // The payload's binding of a `Conv::Payload` (`LocalId(n)` after `this` and the params).
    if let Conv::Payload { have, .. } = conv {
        cx.fn_info_mut(e).local_kinds.push(LocalKind::Bind);
        let Some(Def::Fn(f)) = &mut cx.defs[e.0 as usize] else {
            panic!("ICE: a synthesized method has a body");
        };
        let span = f.span;
        f.body.locals.push(hir::LocalDef {
            name: "<some>".into(),
            ty: have,
            mutable: false,
            boxed: false,
            weak: false,
            span,
        });
    }
    let Some(Def::Fn(f)) = &mut cx.defs[e.0 as usize] else {
        panic!("ICE: a synthesized method has a body");
    };
    let Some(hir::Stmt {
        kind: S::Return(Some(value)),
        span,
    }) = f.body.block.stmts.last_mut()
    else {
        panic!("ICE: a synthesized method returns its call");
    };
    let span: Span = *span;
    let mut call = std::mem::replace(value, unit_placeholder(want, span));
    call.ty = declared;
    // A `this` result declared by a base class is the receiver, an instance of this class (as
    // a call on it is, `body::expr::this_type`).
    if downcast {
        call = hir::Expr {
            kind: H::Downcast(Box::new(call)),
            ty: have,
            span,
        };
    }
    let some = LocalId(f.body.locals.len() as u32 - 1);
    let kind = match conv {
        Conv::Upcast(_) => H::Upcast(Box::new(call)),
        Conv::ToDyn { iface, .. } => {
            pending.push((e, have, iface));
            H::ToDyn {
                expr: Box::new(call),
                impl_index: u32::MAX,
            }
        }
        Conv::Payload {
            iface,
            have,
            want: to,
            whole,
        } => {
            let value = hir::Expr {
                kind: H::Local(some, UseMode::Move),
                ty: have,
                span,
            };
            let kind = match iface {
                Some(iface) => {
                    pending.push((e, have, iface));
                    H::ToDyn {
                        expr: Box::new(value),
                        impl_index: u32::MAX,
                    }
                }
                None => H::Upcast(Box::new(value)),
            };
            let converted = hir::Expr { kind, ty: to, span };
            null_tested(call, (some, have), converted, whole)
        }
    };
    *value = hir::Expr {
        kind,
        ty: want,
        span,
    };
    e
}

/// The class's generics `class` followed by method generics `method`'s own (those after the
/// first `n`, the declaring class's).
fn with_own(class: &Generics, method: &Generics, n: usize) -> Generics {
    let mut g = class.clone();
    let k = method.len().saturating_sub(n);
    g.names.extend(method.names[n..].iter().cloned());
    g.bounds
        .extend(method.bounds.get(n..).unwrap_or_default().iter().cloned());
    let pad = |dst: &mut Vec<Option<TyId>>, src: &[Option<TyId>], len: usize| {
        if dst.is_empty() && src.len() <= n {
            return;
        }
        dst.resize(len, None);
        dst.extend((0..k).map(|i| src.get(n + i).copied().flatten()));
    };
    let len = class.len();
    pad(&mut g.defaults, &method.defaults, len);
    pad(&mut g.constraints, &method.constraints, len);
    g
}

/// Sets the implementation index of each pending entry's `ToDyn`, now that the batch's
/// implementations are built.
pub(super) fn finish(cx: &mut Ctx, pending: Vec<Pending>) {
    for (e, have, iface) in pending {
        let Some(found) = cx.find_impl(have, iface).map(|(i, ..)| i) else {
            let span = cx.fn_info(e).span;
            let (t, i) = (cx.display(have), cx.iface(iface).map(|i| i.name.clone()));
            let i = i.unwrap_or_default();
            cx.error(Diagnostic::error(
                format!("a method whose result converts to the interface's is not supported here yet: `{t}`'s implementation of `{i}` is declared in another batch of modules"),
                span,
            ));
            continue;
        };
        let Some(Def::Fn(f)) = &mut cx.defs[e.0 as usize] else {
            continue;
        };
        let Some(hir::Stmt {
            kind: S::Return(Some(value)),
            ..
        }) = f.body.block.stmts.last_mut()
        else {
            continue;
        };
        if let Some(impl_index) = pending_index(value) {
            *impl_index = found;
        }
    }
}

/// The implementation index `finish` sets in entry result `e`: of its `ToDyn`, or of the one in
/// the `Some` arm of a `Conv::Payload`'s test.
fn pending_index(e: &mut hir::Expr) -> Option<&mut u32> {
    match &mut e.kind {
        H::ToDyn { impl_index, .. } => Some(impl_index),
        H::Match { arms, .. } => arms.first_mut().and_then(|a| pending_index(&mut a.body)),
        H::WrapSome(x) => pending_index(x),
        _ => None,
    }
}

/// `match (value) { some => wrap(converted), null => null }` typed `whole`, with `some` (of
/// type `have`) bound to the payload, as a call converts a nullable value
/// (`body::expr::union_coerce`).
fn null_tested(
    value: hir::Expr,
    (some, have): (LocalId, TyId),
    converted: hir::Expr,
    whole: TyId,
) -> H {
    let span = value.span;
    let pat = |kind, ty| hir::Pat { kind, ty, span };
    let bind = pat(P::Binding(some, UseMode::Move), have);
    let arms = vec![
        hir::Arm {
            pat: pat(P::Some(Box::new(bind)), value.ty),
            guard: None,
            body: hir::Expr {
                kind: H::WrapSome(Box::new(converted)),
                ty: whole,
                span,
            },
        },
        hir::Arm {
            pat: pat(P::None, value.ty),
            guard: None,
            body: unit_placeholder(whole, span),
        },
    ];
    H::Match {
        scrutinee: Box::new(value),
        arms,
    }
}

/// A placeholder for the value being replaced (`null`).
fn unit_placeholder(ty: TyId, span: Span) -> hir::Expr {
    hir::Expr {
        kind: H::Lit(hir::Lit::Null),
        ty,
        span,
    }
}
