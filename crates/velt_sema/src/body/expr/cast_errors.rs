//! The error of a type assertion that does not hold (`x as T`, `body/expr/member.rs`).

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The error of `h as target` that does not assert (`Self::assert_value`).
    pub(super) fn cast_error(&mut self, h: hir::Expr, target: TyId, span: Span) -> hir::Expr {
        let nothings = (self.cx.ty.nothing_of(h.ty), self.cx.ty.nothing_of(target));
        if let (Some(from), Some(to)) = nothings {
            if !from.within(to) && !self.cx.ty.has_error(h.ty) {
                // TypeScript keeps the value (`undefined` stays `undefined`); the type
                // `T | null | undefined` that could hold it comes with phase 1b.
                let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
                self.cx.error(
                    Diagnostic::error(
                        format!("a cast from `{s}` to `{d}` is not supported yet (#930)"),
                        span,
                    )
                    .with_note(format!(
                        "the value would stay `{}`, which `{d}` cannot hold yet; convert it instead (`x ?? {}`)",
                        from.word(),
                        to.word()
                    )),
                );
                return self.error_expr(span);
            }
        }
        if !self.cx.ty.has_error(h.ty) {
            let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
            let diag = Diagnostic::error(format!("cannot cast `{s}` as `{d}`"), span);
            let mut diag = self.cx.with_layout_note(diag, target);
            if let Some(note) = self.cast_refusal_note(h.ty, target) {
                diag = diag.with_note(note);
            }
            if self.cx.has_literal_member(target) {
                diag = diag.with_note(
                    "a value takes a literal type after a test that narrows it (`if (k === \"a\")`)",
                );
            }
            self.cx.error(diag);
        }
        self.error_expr(span)
    }

    /// Why a cast from `from` to `to` is refused, for the shapes TypeScript accepts (one type
    /// assignable to the other) that Velt can't make sound: Velt keeps a value's layout, so a
    /// cast that would change it is no assertion. `None` for other pairs.
    pub(super) fn cast_refusal_note(&mut self, from: TyId, to: TyId) -> Option<String> {
        let (s, d) = (self.cx.display(from), self.cx.display(to));
        // A promise (or a union with one) as `any`: a promise has one owner, so no `any`
        // holds one (`typeof (r as any).then === "function"`, a promise test).
        if self.cx.dynamic(to).is_some() {
            let core = self.cx.ty.core(from).unwrap_or(from);
            let parts = self.cx.union_members(core).unwrap_or_else(|| vec![core]);
            let promise = |s: &Self, t: TyId| matches!(s.cx.ty.kind(t), TyKind::Promise(..));
            if parts.iter().any(|t| promise(self, *t)) {
                return Some(format!(
                    "a promise has one owner, so an `{d}` can't hold one; test for a promise with `instanceof Promise` instead of reading `.then` through `{d}`"
                ));
            }
        }
        // An interface extending the source's, with members the object has no room for.
        if self.related_ifaces(from, to) {
            let have = self.object_members(from).unwrap_or_default();
            let want = self.object_members(to).unwrap_or_default();
            let extra: Vec<String> = (want.iter())
                .filter(|m| !have.iter().any(|h| h.0 == m.0))
                .map(|m| format!("`{}`", m.0))
                .collect();
            if !extra.is_empty() {
                let base = s.split('<').next().unwrap_or(&s).to_string();
                return Some(format!(
                    "`{d}` adds {} to `{s}`: an object keeps the layout it was made with, so one made as a `{s}` has no room for {}; declare {} on `{base}` (optional members are fine) or make the object with {}",
                    extra.join(", "),
                    pronoun(extra.len()),
                    pronoun(extra.len()),
                    pronoun(extra.len()),
                ));
            }
        }
        // The kind of type a cast between instances can't change: a generic type's head, a
        // promise, or a function of so many parameters.
        #[derive(PartialEq)]
        enum Head {
            Generic(hir::DefId),
            Promise,
            Function(usize),
        }
        let head = |k: &TyKind| match k {
            TyKind::Adt(d, a) | TyKind::Dyn(d, a) if !a.is_empty() => Some(Head::Generic(*d)),
            TyKind::Promise(..) => Some(Head::Promise),
            TyKind::FnPtr { params, .. } => Some(Head::Function(params.len())),
            _ => None,
        };
        let (a, b) = (self.cx.ty.kind(from).clone(), self.cx.ty.kind(to).clone());
        match (head(&a), head(&b)) {
            (Some(Head::Function(n)), Some(Head::Function(m))) if n == m => Some(format!(
                "a function value keeps the types it was made with, and a `{s}` can't be called as a `{d}` without converting each argument and its result; wrap it in an arrow that converts them (`(a, b) => f(a as …, b) as …`)"
            )),
            (Some(Head::Promise), Some(Head::Promise)) => Some(format!(
                "a promise keeps the type of the value it settles with, so a `{s}` can't be read as a `{d}`; convert the value where it is awaited (`(await p) as …`)"
            )),
            (Some(x), Some(y)) if x == y => Some(format!(
                "`{s}` and `{d}` are the same type with other type arguments, whose values have other layouts, so the cast would reinterpret the value; convert its members instead, or give both the same type arguments"
            )),
            _ => None,
        }
    }
}

/// "it" or "them", for `n` members.
fn pronoun(n: usize) -> &'static str {
    match n {
        1 => "it",
        _ => "them",
    }
}
