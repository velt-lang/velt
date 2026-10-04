//! Inferred result types (docs/reference/functions.md "Return types"). A function, method or
//! arrow function without a return type returns what its `return` expressions give, as in
//! TypeScript: the one of their types that every other converts to (`i64` and `f64` give `f64`,
//! a class and its base class give the base), else their union; a `return null` makes it
//! nullable; no value gives `void`, only values that never complete give `never`.
//!
//! Callers ask for a function's result with [`ret_of`], which checks the body first when
//! needed (bodies are checked on demand, see `driver`). A use of a function while its own body
//! is being checked is handled by `body::recursion`: only uses that its `return` expressions
//! depend on need a written return type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{ensure_body, FnCx, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, RetSource, RetWant};
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyId};
use crate::visit::{self, VisitMut};

/// The `return`s of a body whose result type is being inferred.
#[derive(Default)]
pub(crate) struct Returns {
    values: Vec<Value>,
    /// `return null` (typed once the result is known).
    nulls: Vec<Span>,
    /// `return;`
    bare: Vec<Span>,
}

struct Value {
    /// Widened type of the returned value (`"a"` → `string`).
    ty: TyId,
    span: Span,
    /// An integer that behaves like a JS number (it converts to a float result).
    inferred_int: bool,
}

/// Bodies checked inside one another to infer a result type, at most (each level takes stack
/// space; a deeper chain of unannotated functions is reported instead of overflowing it).
const MAX_NESTING: usize = 2000;

/// The result type of function `d` (used at `at`), inferring it from the body if needed.
pub(crate) fn ret_of(cx: &mut Ctx, d: DefId, at: Span) -> TyId {
    let f = cx.fn_info(d);
    match f.ret_source.clone() {
        RetSource::Known => f.ret,
        RetSource::Body if f.state == BodyState::InProgress => {
            super::recursion::placeholder(cx, d, at);
            cx.ty.error
        }
        RetSource::Body if f.state == BodyState::Unchecked && cx.checking.len() >= MAX_NESTING => {
            report_too_deep(cx, d, at);
            cx.ty.error
        }
        RetSource::Body => {
            ensure_body(cx, d);
            cx.fn_info(d).ret
        }
        RetSource::Base(base, args) => {
            let r = ret_of(cx, base, at);
            let r = cx.ty.subst(r, &args);
            let f = cx.fn_info_mut(d);
            f.ret = r;
            f.ret_source = RetSource::Known;
            r
        }
    }
}

/// `d`'s result is needed at `at`, inside a chain of bodies checked for their result types
/// that is too long.
fn report_too_deep(cx: &mut Ctx, d: DefId, at: Span) {
    let name = super::recursion::short_name(cx, d);
    cx.error(
        Diagnostic::error(
            format!("inferring the return type of `{name}` nests too many functions"),
            at,
        )
        .with_note(format!(
            "more than {MAX_NESTING} functions without a return type wait for each other here; \
             write the return type of `{name}`"
        )),
    );
}

/// Signature comparisons that waited for inferred results (`collect::ret_infer`).
pub(crate) fn check_deferred(cx: &mut Ctx) {
    for c in std::mem::take(&mut cx.ret_checks) {
        let have = ret_of(cx, c.def, c.span);
        let have = cx.ty.subst(have, &c.args);
        let want = match c.want {
            RetWant::Ty(t) => t,
            RetWant::Of(base, args) => {
                let r = ret_of(cx, base, c.span);
                cx.ty.subst(r, &args)
            }
        };
        if have != want && !cx.ty.has_error(have) && !cx.ty.has_error(want) {
            cx.err(c.message, c.span);
        }
    }
}

impl FnCx<'_, '_> {
    /// `return e` / `return;` in a body whose result type is being inferred.
    pub(super) fn infer_return(&mut self, e: Option<&ast::Expr>, span: Span) -> Option<hir::Expr> {
        let Some(e) = e else {
            self.f.returns.bare.push(span);
            return None;
        };
        if is_null(e) {
            self.f.returns.nulls.push(e.span);
            return Some(self.mk(H::Lit(hir::Lit::Null), self.cx.ty.error, e.span));
        }
        // A value that takes its type from context (`[]`, a number, an arrow) gets the type of
        // an earlier `return`.
        let never = self.cx.ty.never;
        let hint = super::expr::deferred(e)
            .then(|| {
                self.f
                    .returns
                    .values
                    .iter()
                    .map(|v| v.ty)
                    .find(|t| *t != never)
            })
            .flatten();
        let outer = std::mem::replace(&mut self.cx.rec.in_return, true);
        let h = self.expr(e, hint, Want::Move);
        self.cx.rec.in_return = outer;
        let ty = self.cx.widened(h.ty);
        let inferred_int = self.is_inferred_int(&h);
        self.f.returns.values.push(Value {
            ty,
            span: h.span,
            inferred_int,
        });
        Some(h)
    }

    /// The inferred result type of the body `block` (whose `return`s were recorded); converts
    /// every returned value to it. `who` names the function in messages. Also says whether the
    /// result is an integer that behaves like a JS number (`return 1`: `f() / 2` is `0.5`).
    pub(crate) fn finish_inferred_ret(
        &mut self,
        block: &mut hir::Block,
        who: &str,
    ) -> (TyId, bool) {
        let r = std::mem::take(&mut self.f.returns);
        let ty = self.common_ret(&r, block);
        // `T | null` of JS-number integers (`return 5;` and `return null;`) is one too.
        let core = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        let ints = r.values.iter().filter(|v| v.ty == core);
        let inferred_int = self.cx.ty.is_int(core) && ints.clone().count() > 0;
        let inferred_int = inferred_int && ints.clone().all(|v| v.inferred_int);
        self.f.ret = Some(ty);
        let (unit, error) = (self.cx.ty.unit, self.cx.ty.error);
        if ty != unit && ty != error {
            for &span in &r.bare {
                self.bare_return(span, who, ty);
            }
        }
        if !r.nulls.is_empty() || r.values.iter().any(|v| v.ty != ty) {
            visit::block(block, &mut CoerceReturns { fcx: self, to: ty });
        }
        (ty, inferred_int)
    }

    fn common_ret(&mut self, r: &Returns, block: &hir::Block) -> TyId {
        let (never, unit, error) = (self.cx.ty.never, self.cx.ty.unit, self.cx.ty.error);
        let mut tys: Vec<TyId> = vec![];
        for v in &r.values {
            if v.ty != never && !tys.contains(&v.ty) {
                tys.push(v.ty);
            }
        }
        if tys.contains(&error) {
            return error;
        }
        if tys.is_empty() && r.nulls.is_empty() {
            let diverges = crate::flow::block_diverges(block, &self.cx.ty);
            let never_only = !r.values.is_empty() && r.bare.is_empty() && diverges;
            return if never_only { never } else { unit };
        }
        if tys.contains(&unit) {
            if tys.len() == 1 && r.nulls.is_empty() {
                return unit;
            }
            tys.retain(|t| *t != unit);
            // Reported once; the result is unknown, so nothing else is reported about it.
            self.void_returns(r, &tys);
            return error;
        }
        let span = r.nulls.first().or(r.values.first().map(|v| &v.span));
        let span = span.copied().unwrap_or(block.span);
        let core = self.common_type(&tys, &r.values, span);
        if r.nulls.is_empty() || core == error {
            return core;
        }
        let members = if tys.is_empty() { vec![] } else { vec![core] };
        self.cx.union_of(&members, true, span)
    }

    /// A `void` value returned next to other values (`return log(x)` and `return 1`, or
    /// `return null` when `others` is empty).
    fn void_returns(&mut self, r: &Returns, others: &[TyId]) {
        let other = others
            .first()
            .map_or_else(|| "null".to_string(), |t| self.cx.display(*t));
        let unit = self.cx.ty.unit;
        for v in r.values.iter().filter(|v| v.ty == unit) {
            self.cx.error(
                Diagnostic::error("mismatched types", v.span)
                    .with_note(format!("expected {other}, found void")),
            );
        }
    }

    /// The type of `tys` that every other converts to, else their union.
    fn common_type(&mut self, tys: &[TyId], values: &[Value], span: Span) -> TyId {
        for &c in tys {
            let all = tys.iter().all(|&t| {
                let inferred_int = values.iter().filter(|v| v.ty == t).all(|v| v.inferred_int);
                self.converts(t, c, inferred_int)
            });
            if all {
                return c;
            }
        }
        self.cx.union_of(tys, false, span)
    }

    /// Does a value of type `t` convert to `to` implicitly (as `coerce` would)?
    fn converts(&mut self, t: TyId, to: TyId, inferred_int: bool) -> bool {
        if t == to {
            return true;
        }
        if let Some(p) = self.cx.ty.opt_payload(to) {
            return self.converts(t, p, inferred_int);
        }
        if self.cx.ty.is_float(to) && self.cx.ty.is_int(t) {
            return inferred_int;
        }
        if to == self.cx.ty.str_ && self.is_string_enum(t) {
            return true;
        }
        if let Some(ms) = self.cx.union_members(to) {
            return match self.cx.union_members(t) {
                Some(ts) => ts.iter().all(|m| ms.contains(m)),
                None => ms.contains(&t),
            };
        }
        let mut cur = t;
        for _ in 0..64 {
            match self.cx.base_of(cur) {
                Some(b) if b == to => return true,
                Some(b) => cur = b,
                None => return false,
            }
        }
        false
    }

    /// `return;` where the function returns `ty` elsewhere: TS would return `undefined`.
    fn bare_return(&mut self, span: Span, who: &str, ty: TyId) {
        let tn = self.cx.display(ty);
        let nullable = match self.cx.ty.opt_payload(ty) {
            Some(_) => tn.clone(),
            None => format!("{tn} | null"),
        };
        self.cx.error(
            Diagnostic::error(
                format!("`return;` needs a value: {who} returns `{tn}` on other paths"),
                span,
            )
            .with_note(format!(
                "Velt has no `undefined`: write `return null;` and the return type `{nullable}`"
            )),
        );
    }
}

impl FnCx<'_, '_> {
    /// Converts every `return` value of `block` to `to`, the function's result; returns `to`.
    pub(super) fn returns_as(&mut self, block: &mut hir::Block, to: TyId) -> TyId {
        self.f.ret = Some(to);
        visit::block(block, &mut CoerceReturns { fcx: self, to });
        to
    }
}

/// Converts every `return` value of a body to the inferred result type.
struct CoerceReturns<'f, 'a, 'm> {
    fcx: &'f mut FnCx<'a, 'm>,
    to: TyId,
}

impl VisitMut for CoerceReturns<'_, '_, '_> {
    fn stmt(&mut self, s: &mut hir::Stmt) {
        let S::Return(Some(e)) = &mut s.kind else {
            return;
        };
        if matches!(e.kind, H::Lit(hir::Lit::Null)) && e.ty == self.fcx.cx.ty.error {
            e.ty = self.to;
            return;
        }
        if e.ty != self.to {
            let placeholder = self.fcx.mk(H::Lit(hir::Lit::Unit), self.to, e.span);
            let h = std::mem::replace(e, placeholder);
            *e = self.fcx.coerce(h, self.to);
        }
    }
}

/// `null` (possibly parenthesized).
fn is_null(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => true,
        ast::ExprKind::Paren(inner) => is_null(inner),
        _ => false,
    }
}
