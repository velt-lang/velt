//! After inference: written `throws` clauses must allow what the bodies throw, implementations
//! what their interface (or base) method declares, and the error types sema committed to while
//! checking bodies ([`ThrowCheck`]) must agree with the final ones.

use velt_common::{Diagnostic, Span};

use super::groups::Groups;
use super::infer::{member_final, own_final, src_final};
use super::ThrowCheck;
use crate::ctx::Ctx;
use crate::defs::{FnKind, ThrowSrc};
use crate::hir::{DefId, TyId};

pub(super) fn check_all(cx: &mut Ctx, fns: &[DefId], groups: &Groups) {
    for &d in fns {
        if let Some(decl) = cx.fn_info(d).declared_throws {
            let own = own_final(cx, d);
            match cx.error_outside(decl.ty, own) {
                Some(m) if decl.from_body => closure_too_early(cx, d, m),
                Some(m) => clause_violation(cx, d, m, decl.ty, decl.span),
                None => {}
            }
        }
    }
    for g in &groups.list {
        match &g.bound {
            Some(b) => {
                for &m in &g.members {
                    let own = member_final(cx, m);
                    let allowed = super::groups::member_bound(cx, b, m);
                    if let Some(bad) = cx.error_outside(allowed, own) {
                        bound_violation(cx, m, bad, b);
                    }
                }
            }
            None => generic_group(cx, &g.members),
        }
        super::conventions::check_group(cx, g);
    }
    let checks = std::mem::take(&mut cx.throw_checks);
    for c in checks {
        observed(cx, &c);
    }
}

/// Where in `d`'s body the error `m` comes from.
pub(super) fn site_of(cx: &mut Ctx, d: DefId, m: TyId) -> Span {
    let srcs = cx.fn_info(d).throw_srcs.clone();
    first_site(cx, &srcs, m).unwrap_or(cx.fn_info(d).name_span)
}

fn first_site(cx: &mut Ctx, srcs: &[ThrowSrc], m: TyId) -> Option<Span> {
    for s in srcs {
        let t = src_final(cx, s);
        if t.is_some_and(|t| cx.error_members(t).contains(&m)) {
            return Some(s.span());
        }
    }
    None
}

fn clause_violation(cx: &mut Ctx, d: DefId, m: TyId, allowed: Option<TyId>, clause: Span) {
    let at = site_of(cx, d, m);
    let mn = cx.display(m);
    let f = cx.fn_info(d);
    let d = if f.kind == FnKind::Closure {
        let (what, fix) = match allowed {
            Some(t) => (
                format!("allows only `{}`", cx.display(t)),
                format!("add `{mn}` to the function type's `throws`"),
            ),
            None => (
                "does not allow throwing".to_string(),
                format!("write the function type with `throws {mn}`"),
            ),
        };
        let used_as = closure_type(cx, d, allowed);
        Diagnostic::error(
            format!("this function throws `{mn}`, but the function type it is used as {what}"),
            at,
        )
        .with_note(format!("it is used as `{used_as}`"))
        .with_note(format!("catch the error inside the function, or {fix}"))
    } else {
        let name = short_name(&f.name);
        Diagnostic::error(
            format!("`{name}` throws `{mn}`, which its `throws` clause does not allow"),
            at,
        )
        .with_label(clause, "declared here")
        .with_note(format!("add `{mn}` to the `throws` clause, or catch it"))
    };
    cx.error(d);
}

/// The function type closure `d` has (with the errors it may throw), for messages.
fn closure_type(cx: &mut Ctx, d: DefId, allowed: Option<TyId>) -> String {
    let f = cx.fn_info(d);
    let (params, ret, is_async) = (
        f.params.iter().map(|p| p.ty).collect::<Vec<_>>(),
        f.ret,
        f.is_async,
    );
    let never = cx.ty.never;
    let err = allowed.unwrap_or(never);
    let (ret, throws) = match cx.ty.promise_payload(ret).filter(|_| is_async) {
        Some(v) => (cx.ty.promise_rejecting(v, err), never),
        None => (ret, err),
    };
    let t = cx.ty.intern(crate::hir::TyKind::FnPtr {
        params,
        ret,
        throws,
    });
    cx.display(t)
}

/// A closure created without an expected error type took what its body was known to throw;
/// a call inside it reaches a function still being checked (recursion).
fn closure_too_early(cx: &mut Ctx, d: DefId, m: TyId) {
    let at = site_of(cx, d, m);
    let mn = cx.display(m);
    cx.error(
        Diagnostic::error(
            format!("the error type of this function is not known yet: it can throw `{mn}` through a recursive call"),
            at,
        )
        .with_note(format!("write its error type (`(x: T): R throws {mn} => ...`), or add a `throws` clause to the recursive function")),
    );
}

fn bound_violation(cx: &mut Ctx, d: DefId, m: TyId, b: &super::groups::GroupBound) {
    let at = site_of(cx, d, m);
    let mn = cx.display(m);
    let name = short_name(&cx.fn_info(d).name);
    let mut diag = Diagnostic::error(
        format!(
            "`{name}` throws `{mn}`, but `{}` does not allow it",
            short_name(&b.owner)
        ),
        at,
    )
    .with_label(b.decl.span, "the allowed errors are declared here")
    .with_note(
        "implementations and overrides may throw only what the method they implement declares",
    );
    // `throws E` of a generic interface: the implementation's interface arguments choose `E`.
    let param = b.decl.ty.filter(|t| cx.mentions_params(*t));
    if let (Some(iface), Some(p)) = (b.iface.and_then(|i| cx.iface(i)), param) {
        let iname = iface.name.clone();
        let pn = match cx.ty.kind(p) {
            crate::hir::TyKind::Param(i) => iface.generics.names.get(*i as usize).cloned(),
            _ => None,
        };
        if let Some(pn) = pn {
            diag = diag.with_note(format!(
                "`{pn}` is a type argument of `{iname}`: implement `{iname}` with `{mn}` as `{pn}`"
            ));
        }
    }
    cx.error(diag);
}

/// An inferred group error type must not depend on type parameters.
fn generic_group(cx: &mut Ctx, members: &[DefId]) {
    for &m in members {
        let own = member_final(cx, m);
        if own.is_some_and(|t| cx.mentions_params(t)) {
            let name = short_name(&cx.fn_info(m).name);
            let span = cx.fn_info(m).name_span;
            cx.error(
                Diagnostic::error(
                    format!("`{name}` throws an error whose type depends on a type parameter"),
                    span,
                )
                .with_note("it is called through an interface or a vtable, which needs one error type: declare `throws` on the interface (or base) method"),
            );
            return;
        }
    }
}

/// A type sema built while checking bodies must agree with the final inference.
fn observed(cx: &mut Ctx, c: &ThrowCheck) {
    let fin = super::infer::srcs_final(cx, &c.srcs);
    let obs = cx.canon_error(c.observed);
    let ok = if c.exact {
        fin == obs
    } else {
        cx.error_outside(obs, fin).is_none()
    };
    if ok {
        return;
    }
    let fs = fin.map_or("nothing".to_string(), |t| {
        format!("`{}`", shown_errors(cx, t))
    });
    // A call through an interface value: its error type is the interface method's.
    let slot = c.srcs.iter().find_map(|s| match s {
        ThrowSrc::Slot { iface, slot, .. } => {
            let i = cx.iface(*iface)?;
            let m = i.methods.get(*slot as usize)?;
            Some((i.name.clone(), m.name.clone()))
        }
        _ => None,
    });
    let note = match slot {
        Some((i, m)) => {
            let e = fin.map_or("E".to_string(), |t| shown_errors(cx, t));
            format!("add a `throws` clause to interface method `{i}.{m}` (`{m}(): T throws {e};`)")
        }
        None => {
            "add a `throws` clause to the functions in the recursion (`function f(): T throws E`)"
                .into()
        }
    };
    cx.error(
        Diagnostic::error(
            format!(
                "the error type here is not known yet: it is {fs}, found through a recursive call"
            ),
            c.span,
        )
        .with_note(note),
    );
}

/// How many members of an error union a message shows.
const SHOWN_ERRORS: usize = 8;

/// An error type for a message: a long union (growing generic initializers can produce
/// dozens of members, each longer than the last) shows its first members and a count.
fn shown_errors(cx: &mut Ctx, t: TyId) -> String {
    let Some(ms) = cx.union_members(t).filter(|ms| ms.len() > SHOWN_ERRORS) else {
        return cx.display(t);
    };
    let shown: Vec<String> = ms[..SHOWN_ERRORS].iter().map(|m| cx.display(*m)).collect();
    format!(
        "{} | … ({} more)",
        shown.join(" | "),
        ms.len() - SHOWN_ERRORS
    )
}

/// `a.b::C.m` → `C.m`.
pub(super) fn short_name(n: &str) -> String {
    n.rsplit("::").next().unwrap_or(n).to_string()
}
