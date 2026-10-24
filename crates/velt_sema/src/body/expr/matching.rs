//! The null-handling operators, desugared into matches on `T | null`: `x == null` /
//! `x != null` (as values), `a ?? b`, and optional chaining `a?.b`, `a?.m()`, `a?.[i]`, `f?.()`.
//!
//! `a ?? b` yields an owned value: when `a` is a place with a non-Copy payload, the payload is
//! moved out of it (the place is consumed).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::{is_place, place_root, set_place_mode};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Nothing, PatKind as P, TyId, UseMode};

impl FnCx<'_, '_> {
    /// A synthesized binding for a desugared match on `T | null`.
    pub(crate) fn option_binding(
        &mut self,
        s: &hir::Expr,
        payload: TyId,
        name: &str,
        consume: bool,
    ) -> (hir::LocalId, UseMode) {
        let copy = self.cx.is_copy(payload);
        let mode = if copy {
            UseMode::Copy
        } else if is_place(s) && !consume {
            UseMode::Borrow
        } else {
            UseMode::Move
        };
        let l = self.new_local(name, payload, false, s.span, LocalKind::Bind);
        (l, mode)
    }

    fn bool_lit(&self, b: bool, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Bool(b)), self.cx.ty.bool_, span)
    }

    /// `x == null` / `x != null` as a value.
    /// `other == null` / `other == undefined` (`word`), `==` or `===` (`strict`). `==` tests
    /// for either nothing; `===` with the nothing the type lacks (`x === null` on a
    /// `T | undefined`) is constant (`false`, `!==` `true`), as in JavaScript.
    pub(crate) fn null_compare(
        &mut self,
        other: &ast::Expr,
        word: &str,
        strict: bool,
        is_eq: bool,
        span: Span,
    ) -> hir::Expr {
        let mut s = self.test_operand(other, None, Want::Borrow);
        // A narrowed value may be tested again (TS allows it): compare the value itself.
        while let H::UnwrapSome(base, _) = s.kind {
            s = *base;
        }
        self.array_used_in_place(&s);
        if self.cx.dynamic(s.ty).is_some() {
            return self.dynamic_null_compare(s, word, strict, is_eq, span);
        }
        // A type parameter (or a union with one, `T | string`): which nothings the value holds
        // depends on the instantiation (`T = number | null`), so the test is made there.
        if self.param_shaped(s.ty) {
            let b = self.cx.ty.bool_;
            let e = match (strict, word) {
                (true, "null") => self.intrinsic(hir::Intrinsic::IsNull, vec![s], b, span),
                (true, _) => self.undefined_test(s, span),
                _ => self.intrinsic(hir::Intrinsic::IsNullish, vec![s], b, span),
            };
            return self.negated(e, !is_eq, span);
        }
        if self.never_nullish(s.ty) {
            // TS accepts a test of a value that is never nullish (a defensive check).
            return self.never_null_compare(other, s, is_eq, span);
        }
        // An option of a type parameter that allows `undefined` (`x?: T`): which nothings the
        // value can hold depends on the instantiation (`T = string | null` holds `null` too), so
        // the test is made there. (A `T | null` keeps its one test: its `null` is `T`'s too.)
        // So is a union with a type parameter among its members (`T | string | undefined` at
        // `T = number | null` holds `null` as its `T` member, #1268).
        if self.param_payload(s.ty) {
            let union = self.param_union(s.ty);
            match (strict, word, self.cx.ty.nothing_of(s.ty)) {
                (true, "null", Some(Nothing::Undefined)) => {
                    let b = self.cx.ty.bool_;
                    let e = self.intrinsic(hir::Intrinsic::IsNull, vec![s], b, span);
                    return self.negated(e, !is_eq, span);
                }
                (true, "null", Some(_)) if union => {
                    let b = self.cx.ty.bool_;
                    let e = self.intrinsic(hir::Intrinsic::IsNull, vec![s], b, span);
                    return self.negated(e, !is_eq, span);
                }
                (true, "undefined", Some(Nothing::Undefined)) if union => {
                    let e = self.undefined_test(s, span);
                    return self.negated(e, !is_eq, span);
                }
                (false, _, Some(n)) if n.has_undefined() || union => {
                    let b = self.cx.ty.bool_;
                    let e = self.intrinsic(hir::Intrinsic::IsNullish, vec![s], b, span);
                    return self.negated(e, !is_eq, span);
                }
                _ => {}
            }
        }
        match (strict, word, self.cx.ty.nothing_of(s.ty)) {
            // A `T | undefined` is never `null`: still evaluated, then constant. TypeScript
            // accepts it; a warning says so, since an optional parameter or field left out is
            // `undefined`, not `null` (code written when it was `null` tests the wrong one).
            (true, "null", Some(Nothing::Undefined)) => {
                if !self.cx.scopes[self.module].is_std {
                    let tn = self.cx.display(s.ty);
                    let (op, value, loose) = match is_eq {
                        true => ("===", "false", "=="),
                        false => ("!==", "true", "!="),
                    };
                    let mut d = Diagnostic::error(
                        format!("this `{op} null` is always {value}: `{tn}` is never `null`"),
                        span,
                    )
                    .with_note("an optional parameter or field left out is `undefined`, not `null`")
                    .with_note(format!(
                        "write `{loose} null` to test for either, or `{op} undefined`"
                    ));
                    d.severity = velt_common::Severity::Warning;
                    self.cx.error(d);
                }
                self.constant_test(s, !is_eq, span)
            }
            // The `null` of a `T | null` may stand for JavaScript's `undefined` in this phase
            // (an absent optional field, `Map.get`, `find`): which one is not known.
            (true, "undefined", Some(Nothing::Null)) => {
                let tn = self.cx.display(s.ty);
                let op = if is_eq { "===" } else { "!==" };
                let loose = if is_eq { "==" } else { "!=" };
                self.cx.error(
                    Diagnostic::error(
                        format!("`{op} undefined` on a `{tn}` is not supported yet (#930)"),
                        span,
                    )
                    .with_note("its `null` may be JavaScript's `undefined` (an absent optional field, `Map.get`, `find`), which later phases of #930 give their own value")
                    .with_note(format!("to test for either, write `{loose} null`")),
                );
                self.error_expr(span)
            }
            // One nothing of a `T | null | undefined`.
            (true, _, Some(Nothing::Both)) => {
                let which = if word == "null" {
                    Nothing::Null
                } else {
                    Nothing::Undefined
                };
                let sty = s.ty;
                let arms = vec![
                    hir::Arm {
                        pat: self.one_nothing_pat(which, sty, span),
                        guard: None,
                        body: self.bool_lit(is_eq, span),
                    },
                    hir::Arm {
                        pat: self.pat(P::Wildcard, sty, span),
                        guard: None,
                        body: self.bool_lit(!is_eq, span),
                    },
                ];
                let kind = H::Match {
                    scrutinee: Box::new(s),
                    arms,
                };
                self.mk(kind, self.cx.ty.bool_, span)
            }
            _ => self.null_test(s, is_eq, span),
        }
    }

    /// Is `t` an option whose value type is a type parameter, or a union with one among its
    /// members (`T | undefined`, `T | string | undefined`)?
    fn param_payload(&self, t: TyId) -> bool {
        let Some(core) = self.cx.ty.core(t) else {
            return false;
        };
        let members = match self.cx.union_def(core) {
            Some((_, members)) => members,
            None => vec![core],
        };
        members.iter().any(|m| self.instance_valued(*m))
    }

    /// Is `t` a type parameter, or a deferred type standing for one instantiation's value type
    /// (`T[K]`, a generic alias's body): its nothings and `typeof` tag are known only there.
    pub(super) fn instance_valued(&self, t: TyId) -> bool {
        use hir::{TyKind, TyOp};
        matches!(
            self.cx.ty.kind(t),
            TyKind::Param(_) | TyKind::Op(TyOp::Inline(_) | TyOp::Alias(_), _)
        )
    }

    /// Is `t` a union (or an option of one) with a type parameter among its members?
    pub(super) fn param_union(&mut self, t: TyId) -> bool {
        let core = self.cx.ty.core(t).unwrap_or(t);
        let members = self.cx.union_members(core).unwrap_or_default();
        members.iter().any(|m| self.instance_valued(*m))
    }

    /// Is `t` a type parameter, or a union (not an option) with one among its members?
    pub(super) fn param_shaped(&mut self, t: TyId) -> bool {
        if self.cx.ty.opt_payload(t).is_some() {
            return false;
        }
        self.instance_valued(t) || self.param_union(t)
    }

    /// `s === undefined` of a `T | string | undefined` (`param_union`), tested for the
    /// instantiated type: nullish and not `null`.
    pub(super) fn undefined_test(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
        let (sty, b) = (s.ty, self.cx.ty.bool_);
        let (l, mode) = self.option_binding(&s, sty, "<tested>", false);
        let mut s = s;
        if mode == UseMode::Move {
            self.force_move(&mut s);
        }
        let v = |this: &Self| this.mk(H::Local(l, UseMode::Borrow), sty, span);
        let nullish = self.intrinsic(hir::Intrinsic::IsNullish, vec![v(self)], b, span);
        let null = self.intrinsic(hir::Intrinsic::IsNull, vec![v(self)], b, span);
        let not_null = self.negated(null, true, span);
        let test = H::Logical {
            op: hir::LogicOp::And,
            lhs: Box::new(nullish),
            rhs: Box::new(not_null),
        };
        let arms = vec![hir::Arm {
            pat: self.pat(P::Binding(l, mode), sty, span),
            guard: None,
            body: self.mk(test, b, span),
        }];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, b, span)
    }

    /// `s`, evaluated, then `value`.
    pub(super) fn constant_test(&mut self, s: hir::Expr, value: bool, span: Span) -> hir::Expr {
        let sty = s.ty;
        let arms = vec![hir::Arm {
            pat: self.pat(P::Wildcard, sty, span),
            guard: None,
            body: self.bool_lit(value, span),
        }];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// `s == null` / `s != null` of a checked `T | null` value.
    pub(crate) fn null_test(&mut self, s: hir::Expr, is_eq: bool, span: Span) -> hir::Expr {
        let sty = s.ty;
        let arms = if self.is_three_state(sty) {
            let any = self.pat(P::Wildcard, self.present_payload(sty).unwrap_or(sty), span);
            vec![
                hir::Arm {
                    pat: self.present_pat(any, sty, span),
                    guard: None,
                    body: self.bool_lit(!is_eq, span),
                },
                hir::Arm {
                    pat: self.pat(P::Wildcard, sty, span),
                    guard: None,
                    body: self.bool_lit(is_eq, span),
                },
            ]
        } else {
            vec![
                hir::Arm {
                    pat: self.pat(P::None, sty, span),
                    guard: None,
                    body: self.bool_lit(is_eq, span),
                },
                hir::Arm {
                    pat: self.pat(P::Wildcard, sty, span),
                    guard: None,
                    body: self.bool_lit(!is_eq, span),
                },
            ]
        };
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// `a ?? b` → `match (a) { Some(v) => v, null => b }`.
    /// `x!` is `x ?? panic(…)`: TS trusts the assertion, Velt checks it. On a value that is never
    /// `null` it is the value itself, as in TS.
    pub(crate) fn non_null(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        self.non_null_of(inner, exp, want, false, span)
    }

    /// `x!` passed for a borrowed parameter: a variable (or a path of fields) stays where it
    /// is and the callee gets another reference to its value, as it gets the variable itself
    /// for `x` (#1196: `new WeakRef(k!)` keeps its target while `k` lives, as in JS, where the
    /// moved value would be dropped after the call).
    pub(crate) fn non_null_arg(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        self.non_null_of(inner, exp, Want::Borrow, true, span)
    }

    /// `inner!`; with `keep`, a place is not consumed (`non_null_arg`).
    fn non_null_of(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
        keep: bool,
        span: Span,
    ) -> hir::Expr {
        let mut h = self.expr(inner, None, Want::Borrow);
        let payload = self.cx.ty.opt_payload(h.ty);
        if let (Want::Move, H::Global(d)) = (want, &h.kind) {
            // `const v = K!` takes over what `K` names, as `const v = K` does (a table is
            // reported by `force_move`).
            if !crate::body::pure_init::is_table(self.cx, *d) {
                let name = self.cx.global(*d).map(|g| g.name.clone());
                self.moved_out_of_const(*d, payload.unwrap_or(h.ty), span, name);
            }
        }
        if payload.is_none() {
            // `const v = q!` (a `q` known to be present) takes over what `q` names, as
            // `const v = q` does: a place used by value is moved (or shared, when used
            // again), not copied.
            if want == Want::Move && is_place(&h) {
                self.force_move(&mut h);
            }
            return h;
        }
        let msg = "non-null assertion failed: the value is null";
        let read = h.span;
        let h = self.checked_present(h, msg, exp, keep, span);
        self.same_array_read(h.span, read);
        h
    }

    /// The payload of the nullable value `h`, or a panic with `msg` when it is nullish; with
    /// `keep`, a place is borrowed, not consumed.
    pub(crate) fn checked_present(
        &mut self,
        h: hir::Expr,
        msg: &str,
        exp: Option<TyId>,
        keep: bool,
        span: Span,
    ) -> hir::Expr {
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let msg = mk(ast::ExprKind::Lit(ast::Lit::Str(msg.into())));
        let panic = mk(ast::ExprKind::Call {
            callee: Box::new(mk(ast::ExprKind::Ident(ast::Ident {
                name: "panic".into(),
                span,
            }))),
            type_args: vec![],
            args: vec![msg],
            optional: false,
        });
        let keep = keep && is_place(&h) && self.cx.dynamic(h.ty).is_none();
        self.nullish_checked_keeping(h, &panic, exp, keep, span)
    }

    pub(crate) fn nullish(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let s = self.test_operand(lhs, None, Want::Borrow);
        if self.param_shaped(s.ty) {
            return self.param_nullish(s, rhs, span);
        }
        if self.never_nullish(s.ty) {
            // TS accepts `??` on a value that is never nullish: the value itself.
            return self.never_null_nullish(lhs, s, rhs, span);
        }
        self.nullish_checked(s, rhs, exp, span)
    }

    /// `s ?? rhs` of a checked left side (also `s || rhs` on a nullable object).
    pub(crate) fn nullish_checked(
        &mut self,
        s: hir::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        self.nullish_checked_keeping(s, rhs, exp, false, span)
    }

    /// `s ?? rhs`; with `keep`, a place `s` is borrowed, not consumed (its payload, used as a
    /// value, is another reference to it).
    fn nullish_checked_keeping(
        &mut self,
        mut s: hir::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        keep: bool,
        span: Span,
    ) -> hir::Expr {
        let lhs_span = s.span;
        if self.cx.dynamic(s.ty).is_some() {
            return self.dynamic_nullish(s, rhs, span);
        }
        s = self.flatten_chain(s);
        let Some(payload) = self.present_payload(s.ty) else {
            if !self.cx.ty.is_bottom(s.ty) {
                let tn = self.cx.display(s.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("`??` needs a `T | null` value on the left, found `{tn}`"),
                        lhs_span,
                    )
                    .with_note("the left side is never null"),
                );
            }
            self.expr(rhs, exp, Want::Move);
            return self.error_expr(span);
        };
        if payload == self.cx.ty.never {
            return self.always_nullish(s, rhs, exp, span);
        }
        let (l, mode) = self.option_binding(&s, payload, "<nullish>", !keep);
        if mode == UseMode::Move {
            self.force_move(&mut s);
        }
        // An untyped default (`n ?? 0`) takes the payload's type; `x ?? null` and
        // `x ?? undefined` give the payload's option with that nothing (`string | undefined`
        // `?? null` is a `string | null`, as in TypeScript, and so is a `string | null |
        // undefined` `?? null`).
        let hint = match (nothing_lit(rhs), exp) {
            (Some(n), _) => self.cx.ty.option_of(payload, n),
            (_, Some(e)) if !super::ops::untyped(rhs) => e,
            _ => payload,
        };
        let d = self.expr(rhs, Some(hint), Want::Move);
        self.nullish_match(s, l, mode, payload, d, span)
    }

    /// `s ?? rhs` where `s` is `null` or `undefined` alone (an option of `never`): `rhs`, after
    /// `s` is evaluated.
    fn always_nullish(
        &mut self,
        s: hir::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let hint = exp.filter(|_| !super::ops::untyped(rhs));
        let d = self.expr(rhs, hint, Want::Move);
        if matches!(s.kind, H::Lit(_) | H::Local(..)) {
            return d;
        }
        let ty = d.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(s),
                span,
            }],
            value: Some(Box::new(d)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// `s ?? d` for checked HIR operands (an optional field over an earlier one in an object
    /// spread, `spread.rs`).
    pub(crate) fn nullish_exprs(
        &mut self,
        mut s: hir::Expr,
        d: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let payload = self
            .present_payload(s.ty)
            .expect("ICE: `??` of a non-option");
        let (l, mode) = self.option_binding(&s, payload, "<spread>", true);
        if mode == UseMode::Move {
            self.force_move(&mut s);
        }
        self.nullish_match(s, l, mode, payload, d, span)
    }

    /// The match `s ?? d` lowers to, with `l` bound to the payload.
    fn nullish_match(
        &mut self,
        s: hir::Expr,
        l: hir::LocalId,
        mode: UseMode,
        payload: TyId,
        d: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let opt = self.cx.ty.option(payload);
        let mut v = self.mk(H::Local(l, mode), payload, span);
        let two_state = |cx: &crate::ctx::Ctx, t: TyId| {
            cx.ty.core(t) == Some(payload) && cx.ty.nothing_of(t) != Some(Nothing::Both)
        };
        let (ty, d) = if d.ty == opt || two_state(self.cx, d.ty) {
            let dty = d.ty;
            v = self.mk(H::WrapSome(Box::new(v)), dty, span);
            (dty, d)
        } else {
            let (value, d, ty) = self.nullish_join(v, d, payload);
            v = value;
            (ty, d)
        };
        let sty = s.ty;
        let some = self.pat(P::Binding(l, mode), payload, span);
        let (guard, rest) = self.present_guard(sty, l, payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.present_pat(some, sty, span),
                guard,
                body: v,
            },
            hir::Arm {
                pat: rest,
                guard: None,
                body: d,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, ty, span)
    }

    /// The type of `s ?? d` with `v` the non-null payload of `s`: the payload's type when `d`
    /// converts to it, else `d`'s when the payload converts to that, else their union, as in
    /// TypeScript (`n ?? "none"` with `n: number | null` is a `number | string`).
    pub(crate) fn nullish_join(
        &mut self,
        v: hir::Expr,
        d: hir::Expr,
        payload: TyId,
    ) -> (hir::Expr, hir::Expr, TyId) {
        let d = match self.try_coerce(d, payload) {
            Ok(d) => return (v, d, payload),
            Err(d) => d,
        };
        let dty = d.ty;
        let v = match self.try_coerce(v, dty) {
            Ok(v) => return (v, d, dty),
            Err(v) => v,
        };
        let span = d.span;
        let union = self.cx.union_of(&[payload, dty], false, span);
        if union == self.cx.ty.error {
            return (v, d, union);
        }
        let v = self.coerce(v, union);
        let d = self.coerce(d, union);
        (v, d, union)
    }

    /// The guard on the present arm of a match on `sty` (payload `payload` bound to `l`), and
    /// the pattern of the other arm. An option of a type parameter that allows `undefined`
    /// (`x?: T`), or a union with one (`T | Mod<T> | undefined`): at `T = X | null` or
    /// `T = unknown` the present value may be a nothing, which `??` and `?.` take as one, as
    /// in JavaScript. The payload is tested at run time (`Intrinsic::IsNullish`).
    fn present_guard(
        &mut self,
        sty: TyId,
        l: hir::LocalId,
        payload: TyId,
        span: Span,
    ) -> (Option<hir::Expr>, hir::Pat) {
        let generic = self.param_payload(sty)
            && (self.param_union(sty)
                || (self.cx.ty.nothing_of(sty)).is_some_and(|n| n.has_undefined()));
        if !generic {
            return (None, self.nothing_pat(sty, span));
        }
        let b = self.cx.ty.bool_;
        let bound = self.mk(H::Local(l, UseMode::Borrow), payload, span);
        let test = self.intrinsic(hir::Intrinsic::IsNullish, vec![bound], b, span);
        let guard = self.negated(test, true, span);
        (Some(guard), self.pat(P::Wildcard, sty, span))
    }

    /// `object?.<rest>`: apply `f` to the present payload; `null` or `undefined` short-circuits
    /// to `undefined`.
    pub(crate) fn optional_chain(
        &mut self,
        object: &ast::Expr,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let s = self.test_operand(object, None, Want::Borrow);
        let s = self.flatten_chain(s);
        // `any`, and `object` / `Function`, which hold `null` / `undefined` too (issue #926).
        if self
            .cx
            .dynamic(s.ty)
            .is_some_and(|d| d != crate::any::Dynamic::Unknown)
        {
            let (l, mode) = self.option_binding(&s, s.ty, "<chain>", false);
            let use_mode = match mode {
                UseMode::Move => UseMode::Borrow,
                m => m,
            };
            let v = self.mk(H::Local(l, use_mode), s.ty, span);
            let r = f(self, v);
            return self.dynamic_chain_match(s, l, mode, r, span);
        }
        let Some(payload) = self.present_payload(s.ty) else {
            return f(self, s);
        };
        let (l, mode) = self.option_binding(&s, payload, "<chain>", false);
        // A moved-in payload (the object was a temporary) is owned by the binding, which drops
        // it after the rest: the rest uses it as a place, moving out of it only if it consumes it.
        let use_mode = match mode {
            UseMode::Move => UseMode::Borrow,
            m => m,
        };
        let v = self.mk(H::Local(l, use_mode), payload, span);
        let mut r = f(self, v);
        // A binding that borrows the object (a place) does not own the payload: the rest reads
        // through it without moving out of it, so a consumer shares what it reads
        // (`const s = rec?.state` moved the string out of `rec`, which released it again).
        if mode == UseMode::Borrow && place_root(&r) == Some(l) && moves_out(&r) {
            set_place_mode(&mut r, UseMode::Borrow);
        }
        self.chain_match(s, l, mode, payload, r, span)
    }

    /// `match (s) { l => r (wrapped as `T | undefined`), nothing => undefined }`: the short-circuit of
    /// an optional chain whose rest `r` was checked on the payload bound to `l`.
    pub(super) fn chain_match(
        &mut self,
        s: hir::Expr,
        l: hir::LocalId,
        mode: UseMode,
        payload: TyId,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(r.ty) {
            return r;
        }
        // The short-circuit gives `undefined`, on a `T | null` object too, as in JavaScript
        // (issue #930).
        let nothing = Nothing::Undefined;
        let (ty, body) = if nothing == Nothing::Undefined
            && self.cx.ty.nothing_of(r.ty) == Some(Nothing::Null)
        {
            // A `T | null` rest: the chain gives `T | null | undefined`.
            let t = self.cx.ty.option_of(r.ty, Nothing::Undefined);
            (t, self.mk(H::WrapSome(Box::new(r)), t, span))
        } else if r.ty == self.cx.ty.unit || self.cx.ty.opt_payload(r.ty).is_some() {
            (r.ty, r)
        } else {
            let t = self.cx.ty.option_of(r.ty, nothing);
            (t, self.mk(H::WrapSome(Box::new(r)), t, span))
        };
        let none_body = if ty == self.cx.ty.unit {
            self.unit_expr(span)
        } else {
            self.mk(H::Lit(hir::Lit::Null), ty, span)
        };
        let sty = s.ty;
        let some = self.pat(P::Binding(l, mode), payload, span);
        let (guard, rest) = self.present_guard(sty, l, payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.present_pat(some, sty, span),
                guard,
                body,
            },
            hir::Arm {
                pat: rest,
                guard: None,
                body: none_body,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, ty, span)
    }
}

/// Does the place `e` move its value out (its outermost use is `Move`)?
fn moves_out(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Field { mode, .. } | H::Index { mode, .. } | H::UnwrapSome(_, mode) => {
            *mode == UseMode::Move
        }
        H::UnwrapVariant { mode, .. } => *mode == UseMode::Move,
        H::Downcast(x) => moves_out(x),
        _ => false,
    }
}

/// The nothing `e` is written as (`null`, `undefined`), if it is one of those literals.
fn nothing_lit(e: &ast::Expr) -> Option<Nothing> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => Some(Nothing::Null),
        ast::ExprKind::Lit(ast::Lit::Undefined) => Some(Nothing::Undefined),
        ast::ExprKind::Paren(inner) => nothing_lit(inner),
        _ => None,
    }
}
