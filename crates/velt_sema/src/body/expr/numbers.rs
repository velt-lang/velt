//! JS numbers next to Velt's integer types (docs/reference/types.md "Numbers").
//!
//! A `number` is an `f64`, always: an integer literal where no integer type is expected, a
//! local declared from one (unless `literal_locals` types it from its uses), arithmetic on
//! them, and every integer the standard library hands to user code (`xs.length`,
//! `s.indexOf(t)`, `m.size`, `Date.now()`; inside `std/` they keep their declared types).
//! `velt_opt`'s `numrep` stores a number as an integer where it proves that exact.
//!
//! Declared integer types (`i8`..`u64`, `isize`, `usize`) keep Rust semantics. Next to a
//! number, the types whose every value is exactly a double (`i8`..`i32`, `u8`..`u32` and
//! `f32`) convert to it implicitly; the 64-bit ones need `as number`, and a number going into
//! a declared integer needs `as T`. Comparing an integer with a number is exact (`compare`).

use velt_common::Span;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, LocalId, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// Does every value of type `t` convert to a `number` exactly (`i8`..`i32`, `u8`..`u32`,
    /// `f32`)? Those convert implicitly.
    pub(crate) fn exact_in_number(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Int(i) => i.bits() <= 32,
            TyKind::Float(f) => *f == hir::FloatTy::F32,
            _ => false,
        }
    }

    /// Is function `d` part of the JavaScript API the standard library provides (the prelude's
    /// globals: `Array`, `String`, `Map`, `Date`, `Math`, `fetch`, `URL`, …), called from user
    /// code? Its integers are numbers there. Velt's own modules (`velt:sqlite`, `velt:hash`, …)
    /// and its ordering protocol (`compareTo`) keep their declared integer types.
    pub(crate) fn is_js_api(&self, d: DefId) -> bool {
        let f = self.cx.fn_info(d);
        !self.cx.scopes[self.module].is_std
            && self.cx.is_js_api_module(f.module)
            && f.name.rsplit(['.', ':']).next() != Some("compareTo")
    }

    /// An integer the standard library hands to user code: a length, or the result of a JS API
    /// function or method (`is_js_api`) declared with an integer result type (`indexOf`, a
    /// `size` getter, `Date.now()`); not a generic result (`m.get(k)` of a `Map<string, i64>`)
    /// and not a `u64` (Velt-only APIs such as `Math.umulh`).
    pub(crate) fn is_std_api_value(&self, h: &hir::Expr) -> bool {
        if self.cx.scopes[self.module].is_std || !self.cx.ty.is_int(h.ty) {
            return false;
        }
        match &h.kind {
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::ArrayLen | Intrinsic::StrLen),
                ..
            } => true,
            H::Call {
                callee: hir::Callee::Def(d, _),
                ..
            } => {
                let ret = self.cx.fn_info(*d).ret;
                self.is_js_api(*d) && self.cx.ty.is_int(ret) && ret != self.cx.ty.u64
            }
            _ => false,
        }
    }

    /// `h` as user code sees it: an integer from the standard library is a `number`.
    pub(crate) fn std_number(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.is_std_api_value(&h) {
            return h;
        }
        let (f64_, span) = (self.cx.ty.f64, h.span);
        self.mk(H::Cast(Box::new(h)), f64_, span)
    }

    /// The declared (uninstantiated) result type of the JS API function or method `h` calls
    /// from user code (`is_js_api`).
    fn std_result_ty(&self, h: &hir::Expr) -> Option<TyId> {
        match &h.kind {
            H::Call {
                callee: hir::Callee::Def(d, _),
                ..
            } if self.is_js_api(*d) => Some(self.cx.fn_info(*d).ret),
            _ => None,
        }
    }

    /// The integer local `l` (a binding from the standard library) as user code sees it: a
    /// number local of the same name, set from it by the returned statement.
    pub(crate) fn number_shadow(&mut self, l: LocalId) -> hir::Stmt {
        let def = &self.f.locals[l.0 as usize];
        let (name, ty, mutable, span) = (def.name.clone(), def.ty, def.mutable, def.span);
        let kind = if mutable {
            LocalKind::Let
        } else {
            LocalKind::Const
        };
        let f64_ = self.cx.ty.f64;
        let read = self.mk(H::Local(l, UseMode::Copy), ty, span);
        let init = self.mk(H::Cast(Box::new(read)), f64_, span);
        let shadow = self.new_local(&name, f64_, mutable, span, kind);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(name, shadow);
        hir::Stmt {
            kind: hir::StmtKind::Let {
                local: shadow,
                init: Some(init),
            },
            span,
        }
    }

    /// Integer bindings destructured from a standard library result whose signature declares
    /// them integers (`for (const [i, x] of xs.entries())`, `i` from `[usize, T][]`) are
    /// numbers, like the result itself: the statements rebinding them. `elem`: the bindings
    /// take an element of the result apart (a `for … of`), not the result.
    pub(crate) fn std_number_bindings(
        &mut self,
        p: &hir::Pat,
        src: &hir::Expr,
        elem: bool,
    ) -> Vec<hir::Stmt> {
        let Some(mut declared) = self.std_result_ty(src) else {
            return vec![];
        };
        if elem {
            match self.cx.ty.array_elem(declared) {
                Some(e) => declared = e,
                None => return vec![],
            }
        }
        let mut ints = vec![];
        let mut stack = vec![(p, declared)];
        while let Some((p, t)) = stack.pop() {
            match (&p.kind, self.cx.ty.kind(t).clone()) {
                (hir::PatKind::Binding(l, _), _) if self.cx.ty.is_int(t) => ints.push(*l),
                (
                    hir::PatKind::Tuple(ps) | hir::PatKind::Array { elems: ps, .. },
                    TyKind::Tuple(ts),
                ) => {
                    stack.extend(ps.iter().zip(ts));
                }
                (hir::PatKind::Array { elems: ps, .. }, TyKind::Array(e)) => {
                    stack.extend(ps.iter().map(|p| (p, e)));
                }
                _ => {}
            }
        }
        ints.into_iter().map(|l| self.number_shadow(l)).collect()
    }

    /// The one member of the union `t` that `pred` accepts (`None` for other types, or when
    /// none or several do).
    pub(crate) fn union_member(
        &mut self,
        t: TyId,
        pred: impl Fn(&Self, TyId) -> bool,
    ) -> Option<TyId> {
        let ms = self.cx.union_members(t)?;
        let found: Vec<TyId> = ms.into_iter().filter(|m| pred(self, *m)).collect();
        (found.len() == 1).then(|| found[0])
    }

    /// The right operand `v` of `place op= v`: a value of a type that converts to a number
    /// exactly converts to a number place (`total += b` with `b: u8`).
    pub(crate) fn compound_operand(&mut self, place: &hir::Expr, v: hir::Expr) -> hir::Expr {
        if place.ty == self.cx.ty.f64 && self.exact_in_number(v.ty) {
            let f64_ = self.cx.ty.f64;
            return self.int_to_float(v, f64_);
        }
        v
    }

    /// The integer `h` converted to integer type `t`.
    pub(crate) fn int_as(&mut self, h: hir::Expr, t: TyId) -> hir::Expr {
        if h.ty == t {
            return h;
        }
        let span = h.span;
        self.mk(H::Cast(Box::new(h)), t, span)
    }

    /// The number `h` (an integer or `f32` that converts exactly) as a value of float type `t`
    /// (a literal becomes a float literal). A negated literal stays a negated float literal,
    /// so `-0` keeps its sign (#562).
    pub(crate) fn int_to_float(&mut self, h: hir::Expr, t: TyId) -> hir::Expr {
        let span = h.span;
        match h.kind {
            H::Lit(hir::Lit::Int(n)) => self.mk(H::Lit(hir::Lit::Float(n as f64)), t, span),
            H::Unary {
                op: hir::UnOp::Neg,
                expr,
            } if matches!(expr.kind, H::Lit(hir::Lit::Int(_))) => {
                let inner = self.int_to_float(*expr, t);
                let kind = H::Unary {
                    op: hir::UnOp::Neg,
                    expr: Box::new(inner),
                };
                self.mk(kind, t, span)
            }
            kind => {
                let h = hir::Expr { kind, ..h };
                self.mk(H::Cast(Box::new(h)), t, span)
            }
        }
    }

    /// Operands of a binary operator: a value that converts to a number exactly, next to a
    /// number, converts to it (`k * 0.5` with `k: i32`).
    pub(crate) fn mix_numbers(&mut self, l: hir::Expr, r: hir::Expr) -> (hir::Expr, hir::Expr) {
        let f64_ = self.cx.ty.f64;
        if l.ty == f64_ && r.ty != f64_ && self.exact_in_number(r.ty) {
            let r = self.int_to_float(r, f64_);
            (l, r)
        } else if r.ty == f64_ && l.ty != f64_ && self.exact_in_number(l.ty) {
            (self.int_to_float(l, f64_), r)
        } else {
            (l, r)
        }
    }
}

impl FnCx<'_, '_> {
    /// Operands of a comparison between a 64-bit integer and a number, compared exactly. A
    /// number that is an integer from the standard library (`xs.length`) compares as that
    /// integer (`i < xs.length` with `i: usize` compares integers); other numbers compare with
    /// the integer converted (exact within ±2^53). Smaller integers convert exactly
    /// (`mix_numbers`).
    pub(crate) fn compared_numbers(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
    ) -> (hir::Expr, hir::Expr) {
        let f64_ = self.cx.ty.f64;
        let wide = |s: &Self, h: &hir::Expr| s.cx.ty.int_ty(h.ty).is_some_and(|i| i.bits() == 64);
        if wide(self, &l) && r.ty == f64_ {
            let (r, l) = self.int_beside_number(r, l);
            (l, r)
        } else if wide(self, &r) && l.ty == f64_ {
            self.int_beside_number(l, r)
        } else {
            (l, r)
        }
    }

    /// The number `n` and the 64-bit integer `i` of a comparison, as two integers when `n` is
    /// the standard library's integer converted, else as two numbers.
    fn int_beside_number(&mut self, n: hir::Expr, i: hir::Expr) -> (hir::Expr, hir::Expr) {
        let t = i.ty;
        match n.kind {
            H::Cast(inner) if self.is_std_api_value(&inner) => (self.int_as(*inner, t), i),
            kind => {
                let n = hir::Expr { kind, ..n };
                let (f64_, span) = (self.cx.ty.f64, i.span);
                (n, self.mk(H::Cast(Box::new(i)), f64_, span))
            }
        }
    }

    /// The note for a number where a declared 64-bit integer is expected, or the other way
    /// around: the conversion to write.
    pub(crate) fn number_note(&self, expected: TyId, found: TyId) -> Option<String> {
        let ty = &self.cx.ty;
        let (e, f) = (self.cx.display(expected), self.cx.display(found));
        if ty.is_int(expected) && found == ty.f64 {
            return Some(format!("a `number` converts to `{e}` only with `as {e}`"));
        }
        if expected == ty.f64 && ty.is_int(found) && !self.exact_in_number(found) {
            return Some(format!(
                "`{f}` converts to `number` only with `as number` (not every value is exact)"
            ));
        }
        None
    }

    /// The literal `h` (possibly negated) written at `span` (a module constant's value used
    /// there), still a literal for `literal_locals`.
    pub(crate) fn literal_at(&mut self, mut h: hir::Expr, span: Span) -> hir::Expr {
        h.span = span;
        match &mut h.kind {
            H::Lit(hir::Lit::Float(_)) => self.literal_number_lit(span),
            H::Unary { expr, .. } => {
                let inner = std::mem::replace(expr.as_mut(), self.error_expr(span));
                **expr = self.literal_at(inner, span);
            }
            _ => {}
        }
        h
    }

    /// A float index (`xs[i]` with `i: number`, `xs[Math.floor(n / 2)]`) as a `usize`, through
    /// the prelude's `__floatIndex`: a whole number indexes as usual, anything else panics (in
    /// JS it reads `undefined`).
    pub(super) fn float_index(&mut self, h: hir::Expr) -> hir::Expr {
        let Some(crate::ctx::Item::Def(d)) = self.cx.prelude.get("__floatIndex").copied() else {
            return h;
        };
        let span = h.span;
        let f64_ = self.cx.ty.f64;
        let arg = if h.ty == f64_ {
            h
        } else {
            self.mk(H::Cast(Box::new(h)), f64_, span)
        };
        self.mk(
            H::Call {
                callee: hir::Callee::Def(d, vec![]),
                args: vec![arg],
            },
            self.cx.ty.usize,
            span,
        )
    }
}
