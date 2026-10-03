//! Signatures of generators (`function*`, `*name()`; docs/reference/functions.md
//! "Generators"): the declared result must be one of the iteration protocol types the prelude's
//! `Generator<T, E>` class provides — `Generator<T, E>`, `Iterator<T, E>` or `Iterable<T, E>` —
//! or for an async generator (`async function*`, `async *name()`) those of `AsyncGenerator<T,
//! E>`: `AsyncGenerator<T, E>`, `AsyncIterator<T, E>` or `AsyncIterable<T, E>`.
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
    let Some(args) = generator_args(cx, ret, sig.is_async) else {
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

/// `[T, E]` of a generator result type (`Generator`, `Iterator` or `Iterable`; for an async
/// generator `AsyncGenerator`, `AsyncIterator` or `AsyncIterable`).
fn generator_args(cx: &Ctx, ret: TyId, is_async: bool) -> Option<Vec<TyId>> {
    let (_, args, async_) = cx.generator_result_kind(ret)?;
    (args.len() == 2 && async_ == is_async).then_some(args)
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
    let (what, g, it, able) = match sig.is_async {
        true => (
            "an async generator",
            "AsyncGenerator",
            "AsyncIterator",
            "AsyncIterable",
        ),
        false => ("a generator", "Generator", "Iterator", "Iterable"),
    };
    let Some(t) = &sig.ret else {
        cx.error(
            Diagnostic::error(
                format!("{what} must declare its return type"),
                sig.name.span,
            )
            .with_note(format!(
                "write `{g}<T>`, where `T` is the type of the values it yields"
            )),
        );
        return;
    };
    if let Some((_, _, other)) = cx.generator_result_kind(ret) {
        let (fix, tn) = match other {
            true => (
                "make it an async generator: `async function*` (methods: `async *name()`)",
                "async",
            ),
            false => (
                "drop `async`, or declare the result as `AsyncGenerator<T>`",
                "sync",
            ),
        };
        let found = cx.display(ret);
        cx.error(
            Diagnostic::error(
                format!("{what} must return `{g}<T>`, `{it}<T>` or `{able}<T>`, found the {tn} `{found}`"),
                t.span,
            )
            .with_note(fix),
        );
        return;
    }
    let found = cx.display(ret);
    cx.error(
        Diagnostic::error(
            format!("the return type of {what} must be `{g}<T>`, `{it}<T>` or `{able}<T>`, found `{found}`"),
            t.span,
        )
        .with_note(format!(
            "write `{g}<T>`, where `T` is the type of the values it yields (`{g}<{found}>` if it yields `{found}` values)"
        )),
    );
}
