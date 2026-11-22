//! The layout decorators of value types (docs/internals/design/package-machine-code.md §5),
//! declared in std/lang/intrinsics/decorators.velt and found through their roles in the hook
//! table (`decorator_repr`, `decorator_align`), as [`super::known`]'s are:
//!
//! - `@repr("transparent")` on a struct of exactly one field (neither optional nor `void`): the
//!   struct is passed as that field. Between Velt functions it is already (velt_vir passes a
//!   one-field struct as its field), so for them it is a promise; to C it is what crosses: a
//!   `declare c` or `@export` function takes and returns the field's C type
//!   ([`transparent_field`], `collect::c_sig`), and lowering passes the field (velt_vir
//!   `c_call.rs`, `c_export.rs`).
//! - `@align(n)` on a struct: `n` a power of two from 1 to 4096; lowering raises the struct's
//!   alignment to it and pads its size to a multiple of it (velt_vir `layout.rs`).
//!
//! Neither applies to a class, an enum or a `declare c struct` (whose layout is C's), and a
//! transparent struct takes its field's layout, which `@align` would change.

use velt_common::{std_hook, Diagnostic, Span};

use crate::ctx::{Ctx, Item};
use crate::hir::{AdtKind, ConstTree, DecoratorTarget, DefId, TyId, TyKind};

/// The largest `@align`: a page on most targets.
pub(crate) const MAX_ALIGN: i128 = 4096;

/// The role of the decorator whose hook has qualified name `qualified`, when its module is loaded.
fn role(cx: &Ctx, qualified: &str) -> Option<DefId> {
    match cx.std_def(qualified) {
        Some(Item::Def(d)) => Some(d),
        _ => None,
    }
}

/// Is struct `d` declared `@repr("transparent")`? Read from its declaration, so it is known
/// before decorators are checked (C signatures are made with the other signatures): whatever is
/// wrong with the use is reported where uses are checked, and a struct that breaks the rules
/// crosses as nothing ([`transparent_field`]).
fn declared_transparent(cx: &Ctx, d: DefId) -> bool {
    let Some(a) = cx.adt(d) else {
        return false;
    };
    let Some(decl) = a.decl.filter(|t| !t.decorators.is_empty() && !t.c_struct) else {
        return false;
    };
    let Some(want) = role(cx, std_hook!(decorator_repr)) else {
        return false;
    };
    let m = a.module;
    (decl.decorators.iter()).any(|dec| super::known::written_def(cx, m, dec) == Some(want))
}

/// The type of the one field of transparent struct type `t` (its type arguments substituted),
/// if `t` is one that keeps the rules.
pub(crate) fn transparent_field(cx: &mut Ctx, t: TyId) -> Option<TyId> {
    let TyKind::Adt(d, args) = cx.ty.kind(t).clone() else {
        return None;
    };
    if !declared_transparent(cx, d) {
        return None;
    }
    let a = cx.adt(d)?;
    if a.kind != AdtKind::Struct {
        return None;
    }
    let [f] = &a.fields[..] else {
        return None;
    };
    if f.optional {
        return None;
    }
    let declared = f.declared;
    let field = cx.subst(declared, &args);
    (!matches!(cx.ty.kind(field), TyKind::Unit)).then_some(field)
}

/// The uses of `@repr` and `@align` among those recorded for type `d` (`is_enum`: an enum's).
pub(super) fn check(cx: &mut Ctx, d: DefId, is_enum: bool) {
    let Some(uses) = cx.decorator_uses.get(&DecoratorTarget::Def(d)) else {
        return;
    };
    let (repr, align) = (
        role(cx, std_hook!(decorator_repr)),
        role(cx, std_hook!(decorator_align)),
    );
    let find = |want: Option<DefId>| {
        (uses.iter())
            .find(|u| want.is_some_and(|w| u.def == w))
            .map(|u| (u.span, u.args.first().cloned()))
    };
    let (transparent, aligned) = (find(repr), find(align));
    if transparent.is_none() && aligned.is_none() {
        return;
    }
    let kind = match (is_enum, cx.adt(d)) {
        (true, _) => Some("an enum"),
        (false, Some(a)) if a.decl.is_some_and(|t| t.c_struct) => Some("a `declare c struct`"),
        (false, Some(a)) if a.kind == AdtKind::Struct && a.decl.is_some() => None,
        _ => Some("a class"),
    };
    if let Some(kind) = kind {
        for (name, (span, _)) in [("repr", transparent), ("align", aligned)]
            .into_iter()
            .filter_map(|(n, u)| Some((n, u?)))
        {
            let note = match kind {
                "a `declare c struct`" => {
                    "a C struct's layout is C's, which the header check holds it to"
                }
                _ => {
                    "a class is reached through a reference, and an enum's layout is the compiler's"
                }
            };
            cx.error(
                Diagnostic::error(
                    format!("`@{name}` applies to a struct, not to {kind}"),
                    span,
                )
                .with_note(note),
            );
        }
        return;
    }
    if let Some((span, _)) = transparent {
        if let Some((second, _)) = aligned {
            cx.error(
                Diagnostic::error(
                    "a `@repr(\"transparent\")` struct has its field's layout, which `@align` would change",
                    second,
                )
                .with_label(span, "transparent here"),
            );
        }
        transparent_rules(cx, d, span);
    }
    if let Some((span, arg)) = aligned {
        let ok = matches!(arg, Some(ConstTree::Int(n)) if (1..=MAX_ALIGN).contains(&n) && (n as u64).is_power_of_two());
        if !ok {
            let shown = match arg {
                Some(ConstTree::Int(n)) => n.to_string(),
                _ => "this".to_string(),
            };
            cx.error(
                Diagnostic::error(
                    format!("`@align({shown})` is not a power of two from 1 to {MAX_ALIGN}"),
                    span,
                )
                .with_note("an alignment is a power of two (`@align(16)`, `@align(64)`); a page, 4096, is the largest"),
            );
        }
    }
}

/// What a transparent struct `d` (decorated at `span`) must be: exactly one field, neither
/// optional nor `void`.
fn transparent_rules(cx: &mut Ctx, d: DefId, span: Span) {
    let Some(a) = cx.adt(d) else {
        return;
    };
    let name = a.name.clone();
    let fields: Vec<(String, TyId, bool, Span)> = (a.fields.iter())
        .map(|f| (f.name.clone(), f.declared, f.optional, f.span))
        .collect();
    let why = match &fields[..] {
        [] => Some((span, "it has no field".to_string())),
        [(_, _, true, at)] => Some((
            *at,
            "its field is optional, which adds a flag to the struct".to_string(),
        )),
        [(_, ty, _, at)] if matches!(cx.ty.kind(*ty), TyKind::Unit) => {
            Some((*at, "its field is `void`, which takes no space".to_string()))
        }
        [_] => None,
        [_, (_, _, _, at), ..] => Some((*at, format!("it has {} fields", fields.len()))),
    };
    if let Some((at, why)) = why {
        cx.error(
            Diagnostic::error(
                format!("`@repr(\"transparent\")` struct `{name}` has exactly one field: {why}"),
                at,
            )
            .with_label(span, "transparent here")
            .with_note("a transparent struct is passed exactly as its one field, so it has one field that takes space"),
        );
    }
}
