//! The error convention of a dispatch group must be one every member can follow. In a promise
//! group (`Group::promise`) calling an entry never throws: the promises reject, which only an
//! `async` method does, so a synchronous member that can fail is an error. In any other group a
//! call throws the group's errors, so an `async` member (whose call never throws) is an error
//! when the group can fail: that happens when the interface or base method returns a type
//! parameter that an implementation instantiates with a promise.

use velt_common::{Diagnostic, Span};

use super::checks::{short_name, site_of};
use super::groups::{Group, GroupOwner};
use super::infer::member_final;
use crate::ctx::Ctx;
use crate::hir::DefId;

/// Report the members of `g` that cannot follow its error convention.
pub(super) fn check_group(cx: &mut Ctx, g: &Group) {
    let Some(owner) = &g.owner else { return };
    for &m in &g.members {
        let f = cx.fn_info(m);
        // Forwarders synthesized for async methods follow the convention of what they call.
        if f.source.is_none() || f.is_async == g.promise {
            continue;
        }
        let Some(e) = f.throws else { continue };
        let generic_ret = cx.ty.promise_payload(f.ret).is_none();
        let diag = match g.promise {
            true if f.is_getter => getter_cannot_reject(cx, m, e, owner),
            true if generic_ret => generic_cannot_reject(cx, m, e, owner),
            true => must_be_async(cx, m, e, owner),
            false => cannot_be_async(cx, m, e, owner),
        };
        let diag = match failing_member(cx, g, m) {
            Some((at, label)) => diag.with_label(at, label),
            None => diag,
        };
        cx.error(diag);
    }
}

/// `C.m` and `m` of member `d`.
fn names(cx: &Ctx, d: DefId) -> (String, String, Span) {
    let f = cx.fn_info(d);
    let name = short_name(&f.name);
    let method = name.rsplit('.').next().unwrap_or(&name).to_string();
    (name, method, f.name_span)
}

/// A synchronous member of a promise group that can fail.
fn must_be_async(cx: &mut Ctx, d: DefId, e: crate::hir::TyId, owner: &GroupOwner) -> Diagnostic {
    let (name, method, at) = names(cx, d);
    let en = cx.display(e);
    let (why, how) = if owner.interface {
        (
            "it implements an interface method whose promise rejects with".to_string(),
            "a method returning a promise from an interface reports its errors through the promise, which only an `async` method does",
        )
    } else if owner.name == name {
        (
            "it and its overrides return a promise that rejects with".to_string(),
            "an overridden method returning a promise reports its errors through the promise, which only an `async` method does",
        )
    } else {
        (
            format!("`{}` and its overrides return a promise that rejects with", owner.name),
            "an overridden method returning a promise reports its errors through the promise, which only an `async` method does",
        )
    };
    Diagnostic::error(format!("`{name}` must be `async`: {why} `{en}`"), at)
        .with_note(how)
        .with_note(format!("mark it `async {method}(...)`"))
}

/// A getter of a promise group that can fail: getters cannot be `async`.
fn getter_cannot_reject(
    cx: &mut Ctx,
    d: DefId,
    e: crate::hir::TyId,
    owner: &GroupOwner,
) -> Diagnostic {
    let (name, method, at) = names(cx, d);
    let en = cx.display(e);
    Diagnostic::error(
        format!(
            "getter `{name}` cannot fail with `{en}`: `{}` returns a promise, which reports errors by rejecting, and a getter cannot be `async`",
            owner.name
        ),
        at,
    )
    .with_note(format!(
        "catch the error inside the getter, or declare `{method}` as an `async` method"
    ))
}

/// A member of a promise group returning a type parameter (a promise for this group): it
/// cannot be `async`, so it cannot fail.
fn generic_cannot_reject(
    cx: &mut Ctx,
    d: DefId,
    e: crate::hir::TyId,
    owner: &GroupOwner,
) -> Diagnostic {
    let (name, _, at) = names(cx, d);
    let en = cx.display(e);
    Diagnostic::error(
        format!(
            "`{name}` cannot fail with `{en}`: `{}` reports errors by rejecting its promise, and `{name}` returns a type parameter, so it cannot be `async`",
            owner.name
        ),
        at,
    )
    .with_note("catch the error inside it, or implement the method in a class whose return type is a promise")
}

/// An `async` member of a group that throws (its interface or base method does not return a
/// promise).
fn cannot_be_async(cx: &mut Ctx, d: DefId, e: crate::hir::TyId, owner: &GroupOwner) -> Diagnostic {
    let (name, method, at) = names(cx, d);
    let en = cx.display(e);
    let returns = match cx.mentions_params(owner.ret) {
        true => "a type parameter".to_string(),
        false => format!("`{}`", cx.display(owner.ret)),
    };
    Diagnostic::error(
        format!(
            "`{name}` cannot be `async`: calls through `{}`, which returns {returns}, throw `{en}` instead of rejecting a promise",
            owner.name
        ),
        at,
    )
    .with_note(format!(
        "`{}` does not return a promise for every type argument, so all its {} share one entry that throws",
        owner.name,
        if owner.interface { "implementations" } else { "overrides" }
    ))
    .with_note(format!(
        "make `{method}` synchronous (it may still return a promise), or catch the error inside it"
    ))
}

/// Where another member of `g` (or `d` itself) introduces an error, for a label.
fn failing_member(cx: &mut Ctx, g: &Group, d: DefId) -> Option<(Span, String)> {
    let others = g.members.iter().filter(|&&m| m != d);
    for &m in std::iter::once(&d).chain(others) {
        if cx.fn_info(m).source.is_none() {
            continue;
        }
        let Some(e) = member_final(cx, m) else {
            continue;
        };
        let first = cx.error_members(e).first().copied()?;
        let at = site_of(cx, m, first);
        let (name, _, _) = names(cx, m);
        let en = cx.display(first);
        return Some((at, format!("`{name}` fails with `{en}` here")));
    }
    None
}
