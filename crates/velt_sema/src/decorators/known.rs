//! The compile-time decorators the compiler acts on (#2203 step 3), declared in
//! std/lang/intrinsics/decorators.velt and found through their roles in the hook table
//! (`velt_common::std_hooks`: `decorator_inline`, `decorator_no_inline`, `decorator_intrinsic`),
//! never by the name a use is written with: a program's own `declare decorator function inline`
//! is a decorator like any other, which the compiler records and ignores.
//!
//! - `@intrinsic` on a `declare function` makes it a compiler intrinsic ([`intrinsics`], during
//!   collection: a signature's passing modes depend on it). `declare unsafe function` is allowed
//!   only with it.
//! - `@inline` / `@noInline` on a function or method set its `FnInfo::inline_marker`
//!   ([`inline_hint`]), which lowering carries to `vir::Function::inline` (lowering finds the uses
//!   in `Program::decorators` by the same roles) and `body::inline_markers` checks.

use velt_common::{std_hook, Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnKind};
use crate::hir::{DecoratorTarget, DefId};

/// What `@inline` and `@noInline` ask of a function's calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inline {
    /// `@inline`: every call is inlined (`vir::Inline::Always`).
    Always,
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

/// The inlining decorators' definitions, found once per check of a set of definitions.
#[derive(Clone, Copy, Default)]
pub(super) struct InlineRoles {
    inline: Option<DefId>,
    no_inline: Option<DefId>,
}

impl InlineRoles {
    pub(super) fn find(cx: &Ctx) -> Self {
        InlineRoles {
            inline: role(cx, std_hook!(decorator_inline)),
            no_inline: role(cx, std_hook!(decorator_no_inline)),
        }
    }

    fn of(&self, d: DefId) -> Option<Inline> {
        match Some(d) {
            x if x == self.inline => Some(Inline::Always),
            x if x == self.no_inline => Some(Inline::Never),
            _ => None,
        }
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

/// Records `@inline` / `@noInline` among the decorators recorded for function `d` (signature
/// `sig`) on it: one of them, on a function or method with a body that is neither async nor a
/// generator (lowering compiles those into a resume function the hint would not reach).
pub(super) fn inline_hint(cx: &mut Ctx, roles: InlineRoles, d: DefId, sig: &ast::FnSig) {
    let Some(uses) = cx.decorator_uses.get(&DecoratorTarget::Def(d)) else {
        return;
    };
    let hints: Vec<(Inline, Span)> = (uses.iter())
        .filter_map(|u| Some((roles.of(u.def)?, u.span)))
        .collect();
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
        Inline::Always => "inline",
        Inline::Never => "noInline",
    };
    let (kind, is_async) = (cx.fn_info(d).kind, cx.fn_info(d).is_async);
    if kind == FnKind::Extern {
        cx.err(
            format!("`@{name}` applies to a function with a body, not to a `declare function`"),
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
