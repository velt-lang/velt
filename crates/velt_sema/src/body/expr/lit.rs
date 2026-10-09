//! Literals (incl. `null`), string concatenation and template literals.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyId, TyKind};
use crate::types::{int_max, int_name, int_range};

impl FnCx<'_, '_> {
    pub(crate) fn lit(
        &mut self,
        l: &ast::Lit,
        exp: Option<TyId>,
        span: Span,
        negated: bool,
    ) -> hir::Expr {
        let exp = match l {
            ast::Lit::Null => return self.null_lit(exp, span),
            _ => self.hint(exp),
        };
        // A literal expected as a union takes the type of the member it belongs to.
        let exp = exp.map(|t| match self.cx.union_def(t) {
            Some(_) => self
                .lit_member_quiet(
                    t,
                    ast::SignedLit {
                        lit: l.clone(),
                        negative: false,
                    },
                )
                .map_or(t, |(_, m)| m),
            None => t,
        });
        match l {
            ast::Lit::Int { value, suffix } => {
                self.int_lit(*value, suffix.as_deref(), exp, span, negated)
            }
            ast::Lit::Float { value, suffix } => {
                self.float_lit(*value, suffix.as_deref(), exp, span)
            }
            ast::Lit::Str(s) => self.str_lit(s, span),
            ast::Lit::Bool(b) => self.mk(H::Lit(hir::Lit::Bool(*b)), self.cx.ty.bool_, span),
            ast::Lit::Null => unreachable!("ICE: null handled above"),
        }
    }

    /// Type named by a literal suffix (`5u8`, `1.0f32`), if valid for this kind of literal.
    fn suffix_ty(&mut self, s: &str, float_only: bool, span: Span) -> Option<TyId> {
        let t = self.cx.ty.primitive(s).filter(|t| {
            let ty = &self.cx.ty;
            if float_only {
                ty.is_float(*t)
            } else {
                ty.is_numeric(*t)
            }
        });
        if t.is_none() {
            let what = if float_only { "float" } else { "number" };
            self.cx
                .err(format!("invalid suffix `{s}` for {what} literal"), span);
        }
        t
    }

    fn int_lit(
        &mut self,
        value: u128,
        suffix: Option<&str>,
        exp: Option<TyId>,
        span: Span,
        negated: bool,
    ) -> hir::Expr {
        // Without a suffix or an integer type expected, a literal is a number (`literal_locals`
        // may make a local declared from it an integer).
        let ty = match suffix {
            Some(s) => match self.suffix_ty(s, false, span) {
                Some(t) => t,
                None => return self.error_expr(span),
            },
            None => exp
                .filter(|e| self.cx.ty.is_numeric(*e))
                .unwrap_or(self.cx.ty.f64),
        };
        if suffix.is_none() && ty == self.cx.ty.f64 {
            self.literal_number_lit(span);
        }
        if self.cx.ty.is_float(ty) {
            return self.mk(H::Lit(hir::Lit::Float(value as f64)), ty, span);
        }
        let it = self.cx.ty.int_ty(ty).expect("ICE: int literal type");
        if value > int_max(it, negated) {
            let shown = if negated {
                format!("-{value}")
            } else {
                value.to_string()
            };
            self.cx.error(
                Diagnostic::error(format!("literal out of range for `{}`", int_name(it)), span)
                    .with_note(format!(
                        "the literal `{shown}` does not fit into the type `{}` whose range is `{}`",
                        int_name(it),
                        int_range(it)
                    )),
            );
        }
        self.mk(H::Lit(hir::Lit::Int(value)), ty, span)
    }

    fn float_lit(
        &mut self,
        value: f64,
        suffix: Option<&str>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let ty = match suffix {
            Some(s) => match self.suffix_ty(s, true, span) {
                Some(t) => t,
                None => return self.error_expr(span),
            },
            None => exp
                .filter(|e| self.cx.ty.is_float(*e))
                .unwrap_or(self.cx.ty.f64),
        };
        self.mk(H::Lit(hir::Lit::Float(value)), ty, span)
    }

    /// `null` takes its `T | null` type from context.
    fn null_lit(&mut self, exp: Option<TyId>, span: Span) -> hir::Expr {
        match exp {
            Some(t)
                if self
                    .cx
                    .ty
                    .opt_payload(t)
                    .is_some_and(|p| !self.cx.ty.has_error(p)) =>
            {
                self.mk(H::Lit(hir::Lit::Null), t, span)
            }
            Some(t) if t != self.cx.ty.error && !self.cx.ty.has_error(t) => {
                let tn = self.cx.display(t);
                self.cx.error(
                    Diagnostic::error("mismatched types", span)
                        .with_note(format!("expected {tn}, found null"))
                        .with_note(format!("make the type nullable: `{tn} | null`")),
                );
                self.error_expr(span)
            }
            _ => {
                self.cx.error(
                    Diagnostic::error("cannot infer the type of `null`", span)
                        .with_note("add a type annotation like `let x: T | null = null`"),
                );
                self.error_expr(span)
            }
        }
    }

    pub(crate) fn str_lit(&self, s: &str, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Str(s.to_string())), self.cx.ty.str_, span)
    }

    pub(crate) fn intrinsic(
        &self,
        i: Intrinsic,
        args: Vec<hir::Expr>,
        ty: TyId,
        span: Span,
    ) -> hir::Expr {
        self.mk(
            H::Call {
                callee: Callee::Intrinsic(i),
                args,
            },
            ty,
            span,
        )
    }

    pub(crate) fn concat(&self, a: hir::Expr, b: hir::Expr, span: Span) -> hir::Expr {
        self.intrinsic(Intrinsic::StrConcat, vec![a, b], self.cx.ty.str_, span)
    }

    /// Can values of this type be printed / formatted (`console.log`, `${}`)? Everything but
    /// function values, interface values, `void` and shared values — also nested.
    pub(crate) fn printable(&mut self, t: TyId) -> bool {
        self.printable_depth(t, 0)
    }

    fn printable_depth(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 16 {
            return true;
        }
        let parts: Vec<TyId> = match self.cx.ty.kind(t).clone() {
            TyKind::FnPtr { .. }
            | TyKind::Closure(_)
            | TyKind::Dyn(..)
            | TyKind::Unit
            | TyKind::Shared(_) => return false,
            // A promise prints its state and, once settled, its value (`void`: `undefined`).
            TyKind::Promise(v, e) => [v, e]
                .into_iter()
                .filter(|&p| !matches!(self.cx.ty.kind(p), TyKind::Unit))
                .collect(),
            TyKind::Adt(d, args) => {
                // A class printed through its `__inspect()` shows what that returns.
                if let Some((_, ret)) = crate::hooks::hook(self.cx, d, &args, crate::hooks::INSPECT)
                {
                    return self.printable_depth(ret, depth + 1);
                }
                let tys: Vec<TyId> = match &self.cx.info[d.0 as usize] {
                    crate::defs::DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    crate::defs::DefInfo::Enum(e) => {
                        e.variants.iter().flat_map(|v| v.payload.clone()).collect()
                    }
                    _ => vec![],
                };
                tys.into_iter().map(|f| self.cx.subst(f, &args)).collect()
            }
            k => crate::types::children(&k),
        };
        parts
            .into_iter()
            .all(|p| self.printable_depth(p, depth + 1))
    }

    /// A union local read narrowed to printable members (`typeof v === "number"` on
    /// `string | i64 | f64 | () => void`, also with `| null`) as the union of just those members,
    /// so it can be printed; `h` unchanged otherwise.
    pub(crate) fn narrowed_for_print(&mut self, h: hir::Expr) -> hir::Expr {
        if self.printable(h.ty) {
            return h;
        }
        let Some(vs) = self.narrowed_variants(&h) else {
            return h;
        };
        // `T | null` with a narrowed union `T` keeps its `null` (`typeof v !== "function"`).
        let payload = self.cx.ty.opt_payload(h.ty);
        let Some(members) = self.cx.union_members(payload.unwrap_or(h.ty)) else {
            return h;
        };
        let sub: Vec<TyId> = vs.iter().map(|v| members[*v as usize]).collect();
        if sub.is_empty() || !sub.iter().all(|t| self.printable(*t)) {
            return h;
        }
        let span = h.span;
        if payload.is_some() {
            let target = self.cx.union_of(&sub, true, span);
            return self
                .option_to_option_narrowed(h, target, Some(vs))
                .unwrap_or_else(|h| h);
        }
        let target = self.cx.union_of(&sub, false, span);
        let res = if self.cx.union_def(target).is_some() {
            self.coerce_to_union(h, target)
        } else {
            self.union_to_common(h, target)
        };
        res.unwrap_or_else(|h| h)
    }

    pub(crate) fn template(
        &mut self,
        quasis: &[String],
        exprs: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let mut parts = vec![];
        for (i, q) in quasis.iter().enumerate() {
            if !q.is_empty() {
                parts.push(self.str_lit(q, span));
            }
            if let Some(e) = exprs.get(i) {
                let h = self.expr(e, None, Want::Borrow);
                let h = self.unbrand(h);
                let h = self.own_to_string(h, "toString");
                let h = self.narrowed_for_print(h);
                let t = h.ty;
                if t == self.cx.ty.str_ || self.cx.ty.is_bottom(t) {
                    parts.push(h);
                } else if self.printable(t) {
                    let hs = h.span;
                    parts.push(self.intrinsic(Intrinsic::ToString, vec![h], self.cx.ty.str_, hs));
                } else {
                    let tn = self.cx.display(t);
                    self.cx.err(
                        format!("cannot format a value of type `{tn}` in a template literal"),
                        h.span,
                    );
                    parts.push(self.error_expr(h.span));
                }
            }
        }
        self.concat_parts(parts, span)
    }

    /// `h.<method>()` when `h` is a class or struct value with its own `method` (a template
    /// literal uses `toString()`, as in JS; `console.log` a `__inspect()` returning a
    /// `string`), else `h`. A parameter left out takes its default, and a `toString()` result
    /// that is not a `string` is written with `String(result)`, as JS does (`crate::hooks`
    /// reports the shapes this cannot call).
    pub(crate) fn own_to_string(&mut self, h: hir::Expr, method: &str) -> hir::Expr {
        let Some((d, _)) = self.adt_of(h.ty) else {
            return h;
        };
        let Some(m) = self.cx.adt(d).and_then(|a| a.methods.get(method)).copied() else {
            return h;
        };
        crate::body::param_defaults(self.cx, m.def);
        let f = self.cx.fn_info(m.def);
        let plain = !(f.is_async || f.is_generator || f.is_async_gen);
        let to_string = method == crate::hooks::TO_STRING;
        if m.is_static
            || !plain
            || f.params.iter().any(|p| p.default.is_none())
            || (!to_string && f.ret != self.cx.ty.str_)
        {
            return h;
        }
        let span = h.span;
        let Some(call) = self.method_call_hir(h.clone(), method, span) else {
            return h;
        };
        if call.ty == self.cx.ty.str_ || !self.printable(call.ty) {
            return call;
        }
        let str_ = self.cx.ty.str_;
        self.intrinsic(Intrinsic::ToString, vec![call], str_, span)
    }

    /// The string parts of a template literal joined: a left fold of `StrConcat` (one part that
    /// is not a fresh value is concatenated to `""`, so the result is always a fresh string).
    pub(crate) fn concat_parts(&mut self, parts: Vec<hir::Expr>, span: Span) -> hir::Expr {
        let mut it = parts.into_iter();
        let Some(first) = it.next() else {
            return self.str_lit("", span);
        };
        let fresh = matches!(
            first.kind,
            H::Lit(_)
                | H::Call {
                    callee: Callee::Intrinsic(Intrinsic::ToString),
                    ..
                }
        );
        let mut acc = first;
        let mut n = 1;
        for p in it {
            acc = self.concat(acc, p, span);
            n += 1;
        }
        if n == 1 && !fresh {
            // `${s}` alone: always produce a fresh owned string.
            acc = self.concat(self.str_lit("", span), acc, span);
        }
        acc.span = span;
        acc
    }
}
