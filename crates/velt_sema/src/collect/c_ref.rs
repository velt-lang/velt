//! velt/ffi's `Ref<S>` where a C function's signature is written (#2187,
//! docs/internals/design/c-ffi.md): `Ref<S>` resolves to `S`, so only the written type tells a
//! `Ref` parameter (`const S *`, the address of a borrowed copy) from a struct by value. It is
//! read through a program's type aliases, generic ones included, and a `Ref` written anywhere
//! else in a C function's signature is refused rather than cross as the struct.

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::c_sig::{names_ffi_item, path_item};
use crate::ctx::{Ctx, Item};
use crate::hir::{CTy, Nothing, TyId, TyKind};

/// The module of std's `Ref<S>` (std/ffi/ref.velt).
const REF_MODULE: &str = velt_common::std_module!(ffi_ref);

/// How deep aliases are followed: a cycle is reported where it is expanded, this only stops
/// following it.
const MAX_DEPTH: u32 = 32;

/// The type arguments a generic alias was written with: its parameters' names stand for them.
struct Scope<'a> {
    params: &'a [ast::GenericParam],
    args: &'a [ast::TypeExpr],
    /// Where `args` are written, and the scope they are written in.
    module: usize,
    outer: Option<&'a Scope<'a>>,
}

/// What a single-name path stands for in `scope`, if it names one of its parameters: the
/// written argument with the module and scope it is read in, or the parameter's default.
fn param_arg<'a>(
    scope: Option<&'a Scope<'a>>,
    module: usize,
    path: &[ast::Ident],
) -> Option<(&'a ast::TypeExpr, usize, Option<&'a Scope<'a>>)> {
    let (s, [name]) = (scope?, path) else {
        return None;
    };
    let i = s.params.iter().position(|p| p.name.name == name.name)?;
    match s.args.get(i) {
        Some(arg) => Some((arg, s.module, s.outer)),
        None => Some((s.params[i].default.as_ref()?, module, Some(s))),
    }
}

/// Whether the written type `t` (in `module`) is velt/ffi's `Ref<S>` (`Some(false)`) or
/// `Ref<S> | null` (`Some(true)`), named as `StrBytes` is, or through a program's type alias of
/// one (`type R = Ref<S>`, `type Id<T> = T` as `Id<Ref<S>>`, `type M<T> = T | null`).
pub(super) fn written_ref(cx: &Ctx, module: usize, t: &ast::TypeExpr) -> Option<bool> {
    written_in(cx, (module, None), t, 0)
}

fn written_in(
    cx: &Ctx,
    (module, scope): (usize, Option<&Scope>),
    t: &ast::TypeExpr,
    depth: u32,
) -> Option<bool> {
    if depth > MAX_DEPTH {
        return None;
    }
    match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            if let Some((arg, m, s)) = param_arg(scope, module, path).filter(|_| args.is_empty()) {
                return written_in(cx, (m, s), arg, depth + 1);
            }
            if args.len() == 1 && names_ffi_item(cx, module, path, ("Ref", REF_MODULE)) {
                return Some(false);
            }
            let alias = program_alias(cx, module, path)?;
            let inner = Scope {
                params: &alias.decl.generics,
                args,
                module,
                outer: scope,
            };
            written_in(cx, (alias.module, Some(&inner)), &alias.decl.ty, depth + 1)
        }
        // Exactly `Ref<S> | null`, in either order: any other union isn't a `Ref`.
        ast::TypeExprKind::Union(parts) => match parts.as_slice() {
            [a, b] | [b, a]
                if written_in(cx, (module, scope), a, depth + 1) == Some(false)
                    && matches!(b.kind, ast::TypeExprKind::Null) =>
            {
                Some(true)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The alias of the program's own (not std's) that `path` (in `module`) names.
fn program_alias<'c, 'm>(
    cx: &'c Ctx<'m>,
    module: usize,
    path: &[ast::Ident],
) -> Option<&'c crate::defs::AliasInfo<'m>> {
    match path_item(cx, module, path) {
        Some(Item::Alias(a)) => {
            let alias = &cx.aliases[a as usize];
            (!cx.modules[alias.module].is_std).then_some(alias)
        }
        _ => None,
    }
}

/// Whether velt/ffi's `Ref` is written anywhere in `t` (in `module`): in it, its type
/// arguments or the program's aliases it names. Where `t` is no [`written_ref`], such a `Ref`
/// would cross as the struct it resolves to.
pub(super) fn mentions_ref(cx: &Ctx, module: usize, t: &ast::TypeExpr) -> bool {
    mentions_in(cx, module, t, 0)
}

fn mentions_in(cx: &Ctx, module: usize, t: &ast::TypeExpr, depth: u32) -> bool {
    use ast::TypeExprKind as K;
    if depth > MAX_DEPTH {
        return false;
    }
    let any = |ts: &[ast::TypeExpr]| ts.iter().any(|t| mentions_in(cx, module, t, depth + 1));
    match &t.kind {
        K::Named { path, args } => {
            names_ffi_item(cx, module, path, ("Ref", REF_MODULE))
                || any(args)
                || program_alias(cx, module, path)
                    .is_some_and(|a| mentions_in(cx, a.module, &a.decl.ty, depth + 1))
        }
        K::Array(t) | K::TupleRest(t) | K::TupleOptional(t) | K::Weak(t) | K::KeyOf(t) => {
            mentions_in(cx, module, t, depth + 1)
        }
        K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => any(ts),
        K::Function { params, ret, .. } => any(params) || mentions_in(cx, module, ret, depth + 1),
        K::GenericFunction { ty, .. } | K::Construct { ty, .. } => {
            mentions_in(cx, module, ty, depth + 1)
        }
        K::Indexed { object, key } => {
            mentions_in(cx, module, object, depth + 1) || mentions_in(cx, module, key, depth + 1)
        }
        _ => false,
    }
}

/// The error for a `Ref` written in `t` (parameter `what` of C function `name`) that isn't the
/// parameter's whole type.
pub(super) fn misplaced_ref(cx: &mut Ctx, name: &str, what: &str, t: &ast::TypeExpr) {
    cx.error(
        Diagnostic::error(
            format!("{what} of C function `{name}` has a `Ref` that isn't its whole type"),
            t.span,
        )
        .with_note(
            "a C function takes `Ref<S>` or `Ref<S> | null` as a parameter's type, directly or through a type alias; written anywhere else, `Ref<S>` is just `S`",
        ),
    );
}

/// Parameter `p` of C function `name`, of type `ty`, declared `Ref<S>` (or `Ref<S> | null`,
/// `nullable`): the address of a copy of C struct `S`, borrowed for the call (#2187).
pub(super) fn ref_param(
    cx: &mut Ctx,
    (name, p): (&str, &ast::Param),
    ty: TyId,
    nullable: bool,
) -> CTy {
    let inner = match (nullable, cx.ty.kind(ty)) {
        (true, TyKind::Option(inner, Nothing::Null)) => Some(*inner),
        (false, _) => Some(ty),
        _ => None,
    };
    if inner.is_some_and(|t| cx.is_c_struct_ty(t)) {
        return CTy::Ref { nullable };
    }
    let shown = cx.display(ty);
    cx.error(
        Diagnostic::error(
            format!(
                "parameter `{}` of C function `{name}` is a `Ref` of `{shown}`, which is no C struct",
                p.name.name
            ),
            p.ty.span,
        )
        .with_note("`Ref<S>` passes a `declare c struct`'s address (`const S *`); `Ref<S> | null` may pass `NULL`"),
    );
    CTy::Void
}
