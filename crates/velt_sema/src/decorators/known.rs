//! The compile-time decorators the compiler acts on (#2203 step 3), declared in
//! std/lang/intrinsics/decorators.velt and found through their roles in the hook table
//! (`velt_common::std_hooks`: `decorator_inline`, `decorator_no_inline`, `decorator_intrinsic`),
//! never by the name a use is written with: a program's own `declare decorator function inline`
//! is a decorator like any other, which the compiler records and ignores.
//!
//! - `@intrinsic` on a `declare function` makes it a compiler intrinsic ([`intrinsics`], during
//!   collection: a signature's passing modes depend on it). `declare unsafe function` is allowed
//!   only with it; on a function with a body it is an error ([`function_hints`]).
//! - `@inline` / `@noInline` on a function or method of any module (std, a package or the
//!   program; docs/internals/design/package-machine-code.md §1) set its `FnInfo::inline_marker`
//!   ([`function_hints`]), which lowering carries to `vir::Function::inline` (lowering finds the
//!   uses in `Program::decorators` by the same roles) and `body::inline_markers` checks.
//!   `@inline` takes how: `"hint"` (the default) or `"always"`; std writes `"always"`.
//! - `@variant("plain", arg, when)` on a top-level function sets its `FnInfo::variant` ([`variant`]):
//!   a call whose argument `arg` shows what `when` asks takes the function `plain` of the same
//!   module instead (`body::expr::call`, docs/internals/design/call-variants.md).

use velt_common::{std_hook, Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnKind};
use crate::hir::{ConstTree, Decorator, DecoratorTarget, DefId};

/// What `@inline` and `@noInline` ask of a function's calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inline {
    /// `@inline("always")`: every call is inlined (`vir::Inline::Always`).
    Always,
    /// `@inline`, which is `@inline("hint")`: the body should be offered to callers and the
    /// inliner should prefer it. Every function's body is available to its callers while a
    /// program is compiled as one unit, so lowering gives it `vir::Inline::Default` until units
    /// (#2943) carry bodies across.
    Hint,
    /// `@noInline`: calls stay calls (`vir::Inline::Never`).
    Never,
}

/// The definition of the decorator whose hook has qualified name `qualified`, when its module is
/// loaded.
fn role(cx: &Ctx, qualified: &str) -> Option<DefId> {
    match cx.std_def(qualified) {
        Some(Item::Def(d)) => Some(d),
        _ => None,
    }
}

/// The known decorators' definitions, found once per check of a set of definitions.
#[derive(Clone, Copy, Default)]
pub(super) struct Roles {
    inline: Option<DefId>,
    no_inline: Option<DefId>,
    intrinsic: Option<DefId>,
    variant: Option<DefId>,
}

/// A function's `@variant`: the function its calls take when argument `arg` passes `test`.
#[derive(Clone, Debug)]
pub(crate) struct Variant {
    pub plain: DefId,
    pub arg: usize,
    pub test: VariantTest,
}

/// What shows that a call can take a function's variant.
#[derive(Clone, Debug)]
pub(crate) enum VariantTest {
    /// A string literal (or a template literal's text before its first hole) starting with it,
    /// ASCII letters in either case.
    StartsWith(String),
    /// An object literal without this property and without a spread, or no argument.
    Lacks(String),
}

impl VariantTest {
    /// Does argument `arg` (`None`: left out) pass?
    pub(crate) fn passes(&self, arg: Option<&ast::Expr>) -> bool {
        match (self, arg.map(|a| &a.kind)) {
            (VariantTest::StartsWith(p), Some(ast::ExprKind::Lit(ast::Lit::Str(s)))) => {
                starts_with_ci(s, p)
            }
            (VariantTest::StartsWith(p), Some(ast::ExprKind::Template { quasis, .. })) => {
                quasis.first().is_some_and(|q| starts_with_ci(q, p))
            }
            (VariantTest::Lacks(_), None) => true,
            (VariantTest::Lacks(name), Some(ast::ExprKind::Object(props))) => {
                props.iter().all(|p| match p {
                    ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => {
                        k.name != *name
                    }
                    ast::ObjectProp::Method(f) => f.sig.name.name != *name,
                    ast::ObjectProp::Spread(_) => false,
                })
            }
            _ => false,
        }
    }
}

/// Does `s` start with `prefix`, ASCII letters in either case?
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

impl Roles {
    pub(super) fn find(cx: &Ctx) -> Self {
        Roles {
            inline: role(cx, std_hook!(decorator_inline)),
            no_inline: role(cx, std_hook!(decorator_no_inline)),
            intrinsic: role(cx, std_hook!(decorator_intrinsic)),
            variant: role(cx, std_hook!(decorator_variant)),
        }
    }

    fn of(&self, u: &Decorator) -> Option<Inline> {
        match Some(u.def) {
            x if x == self.inline => Some(inline_how(u)),
            x if x == self.no_inline => Some(Inline::Never),
            _ => None,
        }
    }
}

/// What a use `u` of `@inline` asks: its one argument, `"hint"` (the default, filled in) or
/// `"always"`.
fn inline_how(u: &Decorator) -> Inline {
    match u.args.first() {
        Some(ConstTree::Str(s)) if s == "always" => Inline::Always,
        _ => Inline::Hint,
    }
}

/// The `declare function`s from definition `from` on decorated `@intrinsic` become the
/// intrinsics their names say (`intrinsic_decls`); a `declare unsafe function` without it is an
/// error. Run after declaration (imports resolve) and before signatures. A definition without
/// decorators or `unsafe` costs one look.
pub(crate) fn intrinsics<'m>(cx: &mut Ctx<'m>, from: u32) {
    let mut role_def: Option<Option<DefId>> = None;
    for i in from as usize..cx.info.len() {
        let DefInfo::Fn(f) = &cx.info[i] else {
            continue;
        };
        let (FnKind::Extern, Some(sig)) = (f.kind, f.extern_sig) else {
            continue;
        };
        if sig.is_decorator || (sig.decorators.is_empty() && !sig.is_unsafe) {
            continue;
        }
        let m = f.module;
        let want = *role_def.get_or_insert_with(|| role(cx, std_hook!(decorator_intrinsic)));
        let marked = want.is_some_and(|want| {
            (sig.decorators.iter()).any(|dec| written_def(cx, m, dec) == Some(want))
        });
        if marked {
            let declared = crate::intrinsic_decls::declare(cx, m, sig);
            if let DefInfo::Fn(f) = &mut cx.info[i] {
                f.intrinsic = Some(declared);
                f.unsafe_intrinsic = sig.is_unsafe;
            }
        } else if sig.is_unsafe {
            cx.error(
                Diagnostic::error(
                    "only a compiler intrinsic is declared `unsafe`",
                    sig.name.span,
                )
                .with_note("`unsafe` on a declaration is for std's `@intrinsic` ones; calls of a `declare c` function need unsafe code already"),
            );
        }
    }
}

/// The `declare decorator function` decorator `dec` (written in module `m`) names, if it names
/// one; what is wrong with it is reported where its uses are checked.
fn written_def(cx: &Ctx, m: usize, dec: &ast::Decorator) -> Option<DefId> {
    let callee = match &dec.expr.kind {
        ast::ExprKind::Call { callee, .. } => callee,
        _ => &dec.expr,
    };
    let path = super::consts::path_of(callee)?;
    match super::resolve(cx, m, &path) {
        super::Resolved::Decorator(d, _) => Some(d),
        _ => None,
    }
}

/// Checks the known decorators among the decorators recorded for function `d` (signature `sig`,
/// `is_abstract`: an abstract method's): `@intrinsic` is for a `declare function` (which
/// [`intrinsics`] has made one already), and `@inline` / `@noInline` is recorded on it: one of
/// them, on a function or method with a body that is neither async nor a generator (lowering
/// compiles those into a resume function the hint would not reach).
pub(super) fn function_hints(
    cx: &mut Ctx,
    roles: Roles,
    d: DefId,
    sig: &ast::FnSig,
    is_abstract: bool,
) {
    let Some(uses) = cx.decorator_uses.get(&DecoratorTarget::Def(d)) else {
        return;
    };
    let kind = cx.fn_info(d).kind;
    let misplaced = (uses.iter())
        .find(|u| roles.intrinsic.is_some_and(|i| u.def == i) && kind != FnKind::Extern)
        .map(|u| u.span);
    let hints: Vec<(Inline, Span)> = (uses.iter())
        .filter_map(|u| Some((roles.of(u)?, u.span)))
        .collect();
    if let Some(span) = misplaced {
        cx.err("`@intrinsic` applies to a `declare function`", span);
    }
    let Some(&(hint, span)) = hints.first() else {
        return;
    };
    if let Some(&(_, second)) = hints.get(1) {
        cx.error(
            Diagnostic::error("a function is `@inline` or `@noInline`, not both", second)
                .with_label(span, "first given here"),
        );
        return;
    }
    let name = match hint {
        Inline::Always | Inline::Hint => "inline",
        Inline::Never => "noInline",
    };
    let (module, is_async) = (cx.fn_info(d).module, cx.fn_info(d).is_async);
    if hint == Inline::Hint && cx.scopes[module].is_std {
        // std's `@inline` meant "always" before the hint existed; one written without its
        // argument (a pull request from before) would lose its forcing without a word.
        cx.error(
            Diagnostic::error("std's `@inline` says how: `@inline(\"always\")`", span)
                .with_note("in std, a bare `@inline` is reserved: it meant `\"always\"` until packages got the hint (docs/internals/design/package-machine-code.md §1)"),
        );
        return;
    }
    if kind == FnKind::Extern {
        cx.err(
            format!("`@{name}` applies to a function with a body, not to a `declare function`"),
            span,
        );
        return;
    }
    if is_abstract {
        cx.err(
            format!("`@{name}` applies to a function with a body, not to an abstract method"),
            span,
        );
        return;
    }
    if is_async || sig.is_async || sig.is_generator {
        cx.error(
            Diagnostic::error(
                format!("`@{name}` doesn't apply to an async function or a generator"),
                span,
            )
            .with_note("its body is compiled into a function that resumes it, which the decorator would not reach"),
        );
        return;
    }
    cx.fn_info_mut(d).inline_marker = Some((hint, span));
}

/// Records `@variant("plain", arg, when)` on function `d` (signature `sig`): `plain` is a top-level
/// function of the same module with as many parameters and type parameters as `d`, which is a
/// top-level function and not a generator; `arg` names one of the parameters, and `when` has one
/// of `startsWith` and `lacks` (a non-empty string). The call that takes `plain` is checked
/// against `plain`'s own signature.
pub(super) fn variant(cx: &mut Ctx, roles: Roles, d: DefId, sig: &ast::FnSig) {
    let Some(role) = roles.variant else {
        return;
    };
    let Some(uses) = cx.decorator_uses.get(&DecoratorTarget::Def(d)) else {
        return;
    };
    let Some(u) = uses.iter().find(|u| u.def == role).cloned() else {
        return;
    };
    let fail = |cx: &mut Ctx, msg: String| cx.err(msg, u.span);
    let (kind, module, n, generics) = {
        let f = cx.fn_info(d);
        (f.kind, f.module, f.params.len(), f.generics.len())
    };
    if kind != FnKind::Free || sig.is_generator {
        return fail(
            cx,
            "`@variant` applies to a top-level function that is not a generator".into(),
        );
    }
    let (Some(ConstTree::Str(plain)), Some(ConstTree::Int(arg)), Some(ConstTree::Object(when))) =
        (u.args.first(), u.args.get(1), u.args.get(2))
    else {
        return fail(
            cx,
            "`@variant` takes the plain function's name, an argument's index and what shows it"
                .into(),
        );
    };
    let test = match &when[..] {
        [(k, ConstTree::Str(p))] if k == "startsWith" && !p.is_empty() => {
            VariantTest::StartsWith(p.clone())
        }
        [(k, ConstTree::Str(p))] if k == "lacks" && !p.is_empty() => VariantTest::Lacks(p.clone()),
        _ => {
            return fail(
                cx,
                "`@variant`'s last argument is `{ startsWith: \"…\" }` or `{ lacks: \"…\" }`"
                    .into(),
            );
        }
    };
    let arg = match usize::try_from(*arg) {
        Ok(a) if a < n => a,
        _ => {
            return fail(
                cx,
                format!(
                    "`@variant`'s argument index {arg} is not one of the function's {n} parameters"
                ),
            )
        }
    };
    let target = match cx.scopes[module].items.get(plain.as_str()).copied() {
        Some(Item::Def(p)) if p != d && matches!(cx.info[p.0 as usize], DefInfo::Fn(_)) => p,
        _ => {
            return fail(
                cx,
                format!("`@variant` names `{plain}`, which is no function of this module"),
            );
        }
    };
    let t = cx.fn_info(target);
    if t.kind != FnKind::Free
        || t.params.len() != n
        || t.generics.len() != generics
        || t.variant.is_some()
    {
        return fail(cx, format!("`@variant`'s `{plain}` must be a top-level function with the same parameters and type parameters"));
    }
    cx.fn_info_mut(d).variant = Some(Variant {
        plain: target,
        arg,
        test,
    });
}
