//! Signatures of generators (`function*`, `*name()`; docs/reference/functions.md
//! "Generators"): the declared result must be one of the iteration protocol types the prelude's
//! `Generator<T, E>` class provides — `Generator<T, E>`, `Iterator<T, E>` or `Iterable<T, E>`.
//! Like an async function's `Promise<T, E>`, a written `E` is the same as `throws E`: it moves
//! into the declared `throws`, and the signature keeps the result with `E = never`; a call's
//! result carries the generator's final error type (`body/generators.rs`). Parameters are owned
//! (the generator keeps them until it is done), like an async function's.

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::sigs::owned_async_params;
use crate::ctx::Ctx;
use crate::defs::{DeclaredThrows, FnKind, ParamSig};
use crate::hir::TyId;

/// Check and normalize a generator's signature: returns the result type (with `E = never`) and
/// the declared `throws` (a written `E` joined in).
pub(super) fn generator_sig(
    cx: &mut Ctx,
    kind: FnKind,
    ret: TyId,
    throws: Option<DeclaredThrows>,
    sig: &ast::FnSig,
    ps: &mut [ParamSig],
) -> (TyId, Option<DeclaredThrows>) {
    match kind {
        FnKind::Ctor => cx.err("a constructor cannot be a generator", sig.name.span),
        FnKind::Extern => {}
        _ => owned_async_params(cx, ps),
    }
    if sig.is_async {
        cx.error(
            Diagnostic::error("async generators (`async function*`) are not supported yet", sig.name.span)
                .with_note("write a generator (`function*`) that yields promises, or an async function that returns an array"),
        );
    }
    let Some(args) = generator_args(cx, ret) else {
        report_bad_result(cx, ret, sig);
        return (cx.ty.error, throws);
    };
    let written = written_error(sig).then_some(args[1]);
    let throws = match (throws, written) {
        (Some(DeclaredThrows { ty, span, .. }), w) => Some(DeclaredThrows {
            ty: cx.join_errors(ty, w),
            span,
            from_body: false,
        }),
        (None, Some(e)) => Some(DeclaredThrows {
            ty: cx.canon_error(Some(e)),
            span: sig.ret.as_ref().map_or(sig.name.span, |t| t.span),
            from_body: false,
        }),
        (None, None) => None,
    };
    let never = cx.ty.never;
    (cx.with_generator_error(ret, never), throws)
}

/// `[T, E]` of a generator result type (`Generator`, `Iterator` or `Iterable`).
fn generator_args(cx: &Ctx, ret: TyId) -> Option<Vec<TyId>> {
    let (_, args) = cx.generator_result(ret)?;
    (args.len() == 2).then_some(args)
}

/// Is the error type argument written (`Generator<T, E>`), rather than the default?
fn written_error(sig: &ast::FnSig) -> bool {
    matches!(
        sig.ret.as_ref().map(|t| &t.kind),
        Some(ast::TypeExprKind::Named { args, .. }) if args.len() == 2
    )
}

fn report_bad_result(cx: &mut Ctx, ret: TyId, sig: &ast::FnSig) {
    if cx.ty.is_bottom(ret) {
        return;
    }
    let Some(t) = &sig.ret else {
        cx.error(
            Diagnostic::error("a generator must declare its return type", sig.name.span)
                .with_note("write `Generator<T>`, where `T` is the type of the values it yields"),
        );
        return;
    };
    let found = cx.display(ret);
    cx.error(
        Diagnostic::error(
            format!("the return type of a generator must be `Generator<T>`, `Iterator<T>` or `Iterable<T>`, found `{found}`"),
            t.span,
        )
        .with_note(format!(
            "write `Generator<T>`, where `T` is the type of the values it yields (`Generator<{found}>` if it yields `{found}` values)"
        )),
    );
}
