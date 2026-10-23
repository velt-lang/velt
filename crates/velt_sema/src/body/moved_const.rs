//! A module constant evaluated at each use (`export const N: J = { … }`) whose value, or a part
//! of it, is taken over by value: by a variable, a field, a collection or a parameter the callee
//! keeps (`const t = N.a`, `m.set("x", K!.a)`, `new Box(N.a)`, `arr.fill(N.a)`). Each use makes
//! a new object, so the taker would hold a copy where TypeScript shares the one object: the
//! constant is kept in the module state (`crate::kept_consts::keep`), or, when it can't be (it
//! holds a promise), this error is reported, once per constant and span, whether the body check
//! (`FnCx::moved_out_of_const`) or the ownership pass (`ownership::infer::force_move`) finds it.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{DefId, Expr, ExprKind as E, LocalId, Pat, PatKind as P, TyId, TyKind};

/// Is a value of type `ty` one that taking from a module constant changes nothing TypeScript
/// would share: copied (a number, a string, either or `null`), or a function, which has nothing
/// to write?
pub(crate) fn copied_value(cx: &mut Ctx, ty: TyId) -> bool {
    let t = cx.ty.opt_payload(ty).unwrap_or(ty);
    cx.is_copy(t)
        || cx.is_string_value(ty)
        || matches!(cx.ty.kind(t), TyKind::FnPtr { .. } | TyKind::Closure(_))
}

/// The message of the error.
fn message(name: &str) -> String {
    format!("cannot move out of module constant `{name}`")
}

/// Where an error is reported: the constant and where the value taken starts. Forms checked
/// twice over the same source (`Promise.resolve(K!.a)`: the argument, and the conversion around
/// it) start at the same place.
fn key(d: DefId, span: Span) -> (DefId, Span) {
    (
        d,
        Span {
            hi: span.lo,
            ..span
        },
    )
}

/// Was the error for constant `d` at `span` reported (and not rolled back by a trial check)?
pub(crate) fn reported(cx: &Ctx, d: DefId, span: Span) -> bool {
    let Some(&i) = cx.moved_consts.get(&key(d, span)) else {
        return false;
    };
    let Some(msg) = cx.global(d).map(|g| message(&g.name)) else {
        return false;
    };
    cx.diags
        .get(i)
        .is_some_and(|e| e.message == msg && e.labels.first().is_some_and(|l| l.span.lo == span.lo))
}

/// Report `diag`, the error for constant `d` at `span`, once.
pub(crate) fn report(cx: &mut Ctx, d: DefId, span: Span, diag: Diagnostic) {
    if reported(cx, d, span) {
        return;
    }
    cx.moved_consts.insert(key(d, span), cx.diags.len());
    cx.error(diag);
}

/// The error for constant `name` at `span`, where `part` is what is taken (`N.a`, `K!.a`;
/// `None`: a form with no short name, `(K ?? d).a`).
pub(crate) fn error(name: &str, part: Option<&str>, span: Span) -> Diagnostic {
    let what = match part {
        Some(p) => format!("`{p}`"),
        None => format!("this part of `{name}`"),
    };
    let read = match part {
        Some(p) => format!("`{p}`"),
        None => "it".to_string(),
    };
    // A `static readonly` field (`C.K`; statics can't be mutable): a module-level `let`.
    let keep = match name.contains('.') {
        true => "keep the object in a module-level `let`, one shared object".to_string(),
        false => format!("declare `{name}` with `let` to keep one shared object"),
    };
    Diagnostic::error(message(name), span)
        .with_note(format!(
            "`{name}` is evaluated at each use: {what} taken here would be a copy, where TypeScript shares one object"
        ))
        .with_note(format!(
            "read {read} where the value is used instead of keeping it, or {keep}"
        ))
}

/// `e` in source form, for the note (`N.a`, `K!.a`, `ns.K`, `N.a[0]`); `None` for other forms.
pub(crate) fn ast_part(e: &ast::Expr) -> Option<String> {
    Some(match &e.kind {
        ast::ExprKind::Ident(id) => id.name.clone(),
        ast::ExprKind::Paren(x) => return ast_part(x),
        ast::ExprKind::NonNull(x) => format!("{}!", ast_part(x)?),
        ast::ExprKind::Member {
            object,
            prop,
            optional,
        } => {
            let dot = if *optional { "?." } else { "." };
            format!("{}{dot}{}", ast_part(object)?, prop.name)
        }
        ast::ExprKind::Index { object, index, .. } => {
            let i = match &index.kind {
                ast::ExprKind::Lit(ast::Lit::Int { value, .. }) => value.to_string(),
                ast::ExprKind::Lit(ast::Lit::Str(s)) => format!("{s:?}"),
                _ => "…".to_string(),
            };
            format!("{}[{i}]", ast_part(object)?)
        }
        _ => return None,
    })
}

/// The module constant evaluated at each use, not copied, that the value `e` is (a part of),
/// with the part's short form (`N.a`) when it has one: what a parameter the callee keeps takes
/// over (`m.set("x", N.a)`, `new Box(K!.a)`, `put((K ?? d).a)`). A bare `K` is not followed here
/// (`force_move` reports it).
pub(crate) fn hir_root(cx: &mut Ctx, e: &Expr) -> Option<(DefId, Option<String>)> {
    match &e.kind {
        E::Global(d) => {
            let d = *d;
            crate::body::driver::ensure_global(cx, d);
            let g = cx.global(d)?;
            let (stored, ty, name) = (g.stored, g.ty, g.name.clone());
            let per_use = !stored && !cx.is_copy(ty);
            (per_use && !crate::body::pure_init::shares_on_move(cx, d)).then_some((d, Some(name)))
        }
        E::Field { base, index, .. } => {
            let (d, part) = hir_root(cx, base)?;
            let t = cx.ty.opt_payload(base.ty).unwrap_or(base.ty);
            let field = match cx.ty.kind(t) {
                TyKind::Adt(a, _) => cx
                    .adt(*a)
                    .and_then(|a| a.fields.get(*index as usize))
                    .map(|f| f.name.clone()),
                _ => None,
            };
            Some((d, part.zip(field).map(|(p, f)| format!("{p}.{f}"))))
        }
        E::Index { base, .. } => hir_root(cx, base).map(|(d, _)| (d, None)),
        E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::WrapSome(x) => hir_root(cx, x),
        E::If { then, els, .. } => hir_root(cx, then)
            .or_else(|| hir_root(cx, els))
            .map(|(d, _)| (d, None)),
        E::Block(b) => b.value.as_deref().and_then(|t| hir_root(cx, t)),
        E::Match { scrutinee, arms } => {
            for a in arms {
                // `Some(x) => x`, `Some(k) => Some(k.a)` (`K?.a`): the arm gives (a part of)
                // the scrutinee.
                let binds = local_root(&a.body).is_some_and(|m| pat_binds(&a.pat, m));
                let found = match binds {
                    true => hir_root(cx, scrutinee),
                    false => hir_root(cx, &a.body),
                };
                if let Some((d, _)) = found {
                    return Some((d, None));
                }
            }
            None
        }
        _ => None,
    }
}

/// The local that `e` is (a part of), through fields, elements and wrappers (`Some(k.a)`).
fn local_root(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        E::Local(l, _) => Some(*l),
        E::Field { base: x, .. }
        | E::Index { base: x, .. }
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::WrapSome(x) => local_root(x),
        _ => None,
    }
}

/// Does `p` bind local `l`?
fn pat_binds(p: &Pat, l: LocalId) -> bool {
    match &p.kind {
        P::Binding(b, _) => *b == l,
        P::Variant { args: ps, .. } | P::Tuple(ps) | P::Or(ps) => {
            ps.iter().any(|q| pat_binds(q, l))
        }
        P::Array { elems, rest } => *rest == Some(l) || elems.iter().any(|q| pat_binds(q, l)),
        P::Adt { fields } => fields.iter().any(|(_, q)| pat_binds(q, l)),
        P::Some(q) => pat_binds(q, l),
        P::Wildcard | P::Lit(_) | P::None | P::InstanceOf(_) => false,
    }
}
