//! The walk of one live definition's checked code ([`super`]): the definitions it names, the
//! types it has values of, and the constructs in it.

use velt_common::std_hooks::Construct;

use super::{constructs, kind_bit, Walk};
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, Callee, DefId, ExprKind as E, Intrinsic as I, TyId, TyKind};

/// Walk the checked code of live definition `d` into `w`; false if it is not checked yet.
pub(super) fn walk_def(cx: &mut Ctx, d: DefId, w: &mut Walk) -> bool {
    // Only a check that made stubs changes the code it walks ([`rebind_stubs`]); otherwise the
    // walk reads it, and a frozen unit's definitions are never written.
    if cx.lazy.stubs.any() && !rebind_stubs(cx, d, w) {
        return false;
    }
    let i = d.0 as usize;
    let ty = &cx.ty;
    if let DefInfo::Fn(f) = &cx.info[i] {
        w.async_host |= f.host.is_some() && f.is_async;
    }
    let visit = |e: &hir::Expr, w: &mut Walk| walk_expr(ty, e, w);
    match (&cx.defs[i], &cx.info[i]) {
        (Some(hir::Def::Fn(f)), info) => {
            if let DefInfo::Fn(fi) = info {
                for e in fi.params.iter().filter_map(|p| p.default.as_ref()) {
                    crate::visit_ref::exprs_in(e, &mut |e| visit(e, w));
                }
                // Thrown and not caught, it is reported as `console.log` prints it: lowering
                // builds that from the signature, whatever the body throws. (`hir::FnDef::throws`
                // is filled in later, by `finalize::build_defs`.)
                w.formats
                    .extend(fi.declared_throws.as_ref().and_then(|d| d.ty));
            }
            for t in f.body.locals.iter().map(|l| l.ty).chain([f.ret]) {
                note_ty(ty, t, w);
            }
            if f.is_generator {
                w.generators[f.is_async as usize] = true;
            }
            w.tasks |= f.is_async;
            crate::visit_ref::exprs(&f.body.block, &mut |e| visit(e, w));
            true
        }
        (None, DefInfo::Fn(_)) => false,
        (_, DefInfo::Global(g)) => {
            if g.state != crate::defs::BodyState::Done {
                return false;
            }
            note_ty(ty, g.ty, w);
            if let Some(e) = &g.init {
                crate::visit_ref::exprs_in(e, &mut |e| visit(e, w));
            }
            true
        }
        (Some(hir::Def::ExternFn(x)), _) => {
            // The runtime's declaration, not the executor's own `declare c` of the symbol.
            let sched = velt_common::std_hooks::SCHED_EXTERNS.contains(&x.symbol.as_str());
            w.tasks |= sched && x.c.is_none();
            true
        }
        _ => true,
    }
}

/// In a check that made stubs, the calls of bound stubs in live definition `d`'s code (its
/// parameters' defaults, body, initializer) and its throw sources name the hooks' definitions;
/// a call of an unbound one is noted in `w.stub_calls`. False, as [`walk_def`] answers, for a
/// global not yet checked, whose walk drops what the batch noted so far, as it always has.
fn rebind_stubs(cx: &mut Ctx, d: DefId, w: &mut Walk) -> bool {
    let i = d.0 as usize;
    let stubs = &cx.lazy.stubs;
    if let DefInfo::Fn(f) = &mut cx.info[i] {
        stubs.rebind_throws(&mut f.throw_srcs);
    }
    let mut stub_calls = std::mem::take(&mut w.stub_calls);
    let mut note = |e: &mut hir::Expr| stubs.note(d, e, &mut stub_calls);
    match (&mut cx.defs[i], &mut cx.info[i]) {
        (Some(hir::Def::Fn(f)), info) => {
            if let DefInfo::Fn(fi) = info {
                for e in fi.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                    crate::visit::exprs_in(e, &mut note);
                }
            }
            crate::visit::exprs_mut(&mut f.body.block, &mut note);
        }
        (_, DefInfo::Global(g)) => {
            if g.state != crate::defs::BodyState::Done {
                return false;
            }
            if let Some(e) = &mut g.init {
                crate::visit::exprs_in(e, &mut note);
            }
        }
        _ => {}
    }
    w.stub_calls = stub_calls;
    true
}

/// Record type `t` of live code: a scalar by its kind only (it opens implementations and has
/// no parts), anything else to [`super::live_types`].
#[inline]
fn note_ty(ty: &crate::types::Types, t: TyId, w: &mut Walk) {
    match ty.kind(t) {
        k @ (TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Bool
        | TyKind::Str
        | TyKind::Symbol
        | TyKind::Simd(_)
        | TyKind::Literal(_)) => w.kinds |= kind_bit(k),
        TyKind::Unit | TyKind::Never | TyKind::Error => {}
        _ => {
            if w.last_type != Some(t) {
                w.last_type = Some(t);
                w.types.push(t);
            }
        }
    }
}

pub(super) fn walk_expr(ty: &crate::types::Types, e: &hir::Expr, w: &mut Walk) {
    note_ty(ty, e.ty, w);
    match &e.kind {
        E::Call {
            callee: Callee::Def(d, targs),
            ..
        }
        | E::FnRef(d, targs) => {
            w.refs.push(*d);
            w.types.extend(targs);
        }
        E::Global(d) | E::Closure(d) => w.refs.push(*d),
        E::AdtLit { type_args, .. } | E::Variant { type_args, .. } | E::New { type_args, .. } => {
            w.types.extend(type_args)
        }
        E::ToDyn { expr, .. } => {
            w.formats.push(expr.ty);
            w.into_any.push((expr.ty, e.ty));
        }
        E::Throw(v) => w.formats.push(v.ty),
        E::Call {
            callee: Callee::Intrinsic(I::ToString | I::TemplateToString),
            args,
        } => w.texts.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Intrinsic(I::Print | I::PrintErr),
            args,
        } => {
            // A float is written by `numberWrite`, which waits for this (`number_text.rs`); a
            // type parameter's instance may be one.
            let float =
                |k: &TyKind| matches!(k, TyKind::Float(_) | TyKind::Param(_) | TyKind::Op(..));
            w.prints_float |= (args.iter()).any(|a| float(ty.kind(a.ty)));
            w.formats.extend(args.iter().map(|a| a.ty));
        }
        E::Call {
            callee: Callee::Intrinsic(I::Inspect | I::InspectNested | I::ErrorText),
            args,
        } => w.formats.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Intrinsic(I::AnyTo | I::AnyToExact),
            ..
        } => w.conversions.push(e.ty),
        E::Call {
            callee: Callee::Intrinsic(I::CatchUnwind | I::CatchUnwindAt),
            ..
        } => w.constructs |= Construct::CatchUnwind.bit(),
        E::Call {
            callee: Callee::Dyn {
                method_type_args, ..
            },
            ..
        } => w.types.extend(method_type_args),
        E::Call {
            callee:
                Callee::ParamMethod {
                    iface,
                    method_type_args,
                    iface_args,
                    ..
                },
            ..
        } => {
            w.ifaces.push(*iface);
            w.types.extend(method_type_args.iter().chain(iface_args));
        }
        kind => w.constructs |= constructs::pow_bit(ty, kind),
    }
}
