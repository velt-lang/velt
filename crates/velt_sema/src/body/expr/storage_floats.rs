//! The storage floats `f16` and `bf16` in expressions (#2049): an `f16`/`bf16` is a `number`
//! with 16-bit storage, as a `Float16Array` element is in JavaScript. Reading one widens it to
//! `number` (f64, exactly), so every operator computes in f64 and gives a `number`; storing
//! into one rounds once (`=`, compound assignment); a literal of one is its f64 value rounded
//! once (an error where that is infinite), and only `as` narrows a value into one.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` widened to `number` (f64, exactly) if it is an `f16` or `bf16`: what reading one
    /// gives (#2049); `h` as it is otherwise.
    pub(crate) fn storage_widened(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.cx.ty.is_storage_float(h.ty) {
            return h;
        }
        let (f64_, span) = (self.cx.ty.f64, h.span);
        self.mk(H::Cast(Box::new(h)), f64_, span)
    }

    /// Whether a literal of type `ty` is one of a storage float, reporting it if it rounds to
    /// infinity.
    pub(super) fn storage_literal(&mut self, value: f64, ty: TyId, span: Span) -> bool {
        let storage = self.cx.ty.is_storage_float(ty);
        if storage {
            self.storage_literal_range(value, ty, span);
        }
        storage
    }

    /// Reports a literal of a storage float type (`f16`, `bf16`) that rounds to infinity
    /// (#2049): the literal is the f64 value rounded once to the type, as `as` rounds.
    pub(super) fn storage_literal_range(&mut self, value: f64, ty: TyId, span: Span) {
        use velt_common::half;
        let (bits, inf, max) = match self.cx.ty.kind(ty) {
            TyKind::Float(hir::FloatTy::F16) => {
                (half::f64_to_f16(value.to_bits()), 0x7c00, "65504")
            }
            _ => (
                half::f64_to_bf16(value.to_bits()),
                0x7f80,
                "3.3895313892515355e38",
            ),
        };
        if bits & 0x7fff == inf {
            let name = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(format!("literal out of range for `{name}`"), span).with_note(
                    format!(
                    "the literal `{}` rounds to infinity: the largest finite `{name}` is `{max}`",
                    if value.abs() >= 1e21 {
                        format!("{value:e}")
                    } else {
                        value.to_string()
                    }
                ),
                ),
            );
        }
    }

    /// `x as f16` on an `any` (or `unknown`): the number it holds (`TypeError` otherwise),
    /// rounded once (#2049).
    pub(super) fn any_as_storage_cast(
        &mut self,
        inner: hir::Expr,
        target: TyId,
        span: Span,
    ) -> hir::Expr {
        let f64_ = self.cx.ty.f64;
        let n = self.cast_dynamic(inner, f64_);
        self.mk(H::Cast(Box::new(n)), target, span)
    }

    /// An `any` (or `unknown`) where an `f16`/`bf16` is expected: narrowing needs `as` (#2049),
    /// which checks that it holds a number and rounds it.
    pub(super) fn any_as_storage(&mut self, target: TyId, span: Span) -> hir::Expr {
        let d = self.cx.display(target);
        let msg =
            format!("an `any` doesn't convert to `{d}` implicitly: write `x as {d}` (rounds)");
        self.cx.err(msg, span);
        self.error_expr(span)
    }

    /// `place op= v` (or `place++`) on an `f16`/`bf16` place (#2049): `place = (place op v) as
    /// f16`, the place read as a `number` and the operation in f64 (bitwise operators with JS's
    /// ToInt32), the result rounded once on the store, as a `Float16Array` element's compound
    /// assignment does. The place's object and indices are evaluated once, before `v`. `v` is
    /// any value that converts to `number` exactly. `None` after reporting.
    pub(super) fn storage_rmw(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        v: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> Option<hir::Expr> {
        self.storage_rmw_kept(place, op, v, stmts, false, span)
            .map(|(a, _)| a)
    }

    /// [`Self::storage_rmw`]; with `keep`, the f64 result is bound to a temporary (in `stmts`)
    /// and returned too: the value of the compound assignment used as an expression, which JS
    /// gives unrounded.
    pub(super) fn storage_rmw_kept(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        v: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
        keep: bool,
        span: Span,
    ) -> Option<(hir::Expr, Option<hir::LocalId>)> {
        let lty = place.ty;
        let f64_ = self.cx.ty.f64;
        let v = self.storage_widened(v);
        let v = if v.ty == f64_ || self.cx.ty.is_bottom(v.ty) {
            v
        } else if self.exact_in_number(v.ty) {
            self.int_to_float(v, f64_)
        } else {
            let vt = self.cx.display(v.ty);
            let mut d = Diagnostic::error(
                format!("mismatched types: expected number, found {vt}"),
                span,
            );
            if let Some(note) = self.number_note(f64_, v.ty) {
                d = d.with_note(note);
            }
            self.cx.error(d);
            return None;
        };
        let mut place = place;
        self.hoist_indices(&mut place, stmts);
        // As the number path: the object a narrowed field held before `v` (#1280).
        self.hoist_checked_object(&mut place, &v, stmts);
        let cur = self.place_read(&place, crate::body::Want::Borrow);
        let cur = self.storage_widened(cur);
        let wide = match super::ops::hir_binop(op) {
            _ if super::int32::is_bitwise(op) => self.js_bitwise_assign(op, cur, v, span),
            Some(hir::BinOp::Div) => self.divide(cur, v, f64_, span),
            Some(op) => {
                let kind = H::Binary {
                    op,
                    lhs: Box::new(cur),
                    rhs: Box::new(v),
                };
                self.mk(kind, f64_, span)
            }
            None => return None,
        };
        let (wide, kept) = match keep {
            true => {
                let l = self.bind_temp("<result>", wide, stmts);
                (
                    self.mk(H::Local(l, hir::UseMode::Copy), f64_, span),
                    Some(l),
                )
            }
            false => (wide, None),
        };
        let narrowed = self.mk(H::Cast(Box::new(wide)), lty, span);
        // A held tuple `v` cleared is reported as on the number path (#1352).
        let narrowed = self.held_rechecked(narrowed);
        let unit = self.cx.ty.unit;
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(narrowed),
        };
        Some((self.mk(kind, unit, span), kept))
    }

    /// `w = L` (a number literal) or `w op= v` on an `f16`/`bf16` place used as a value (#2049):
    /// the value is the `number` before the store rounds it, as JS gives for a `Float16Array`
    /// element (`(e[0] = 0.1)` is 0.1). `Err(place)` for any other `=`, whose value (of the
    /// place's type) reads back the same.
    pub(super) fn storage_assign_value(
        &mut self,
        place: hir::Expr,
        op: Option<ast::BinaryOp>,
        value: &ast::Expr,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let f64_ = self.cx.ty.f64;
        let lty = place.ty;
        let mut stmts = Vec::new();
        let kept = match op {
            None if plain_literal(value).is_some() => {
                if let Some(n) = plain_literal(value) {
                    self.storage_literal_range(n, lty, value.span);
                }
                let v = self.expr(value, Some(f64_), crate::body::Want::Borrow);
                let l = self.bind_temp("<assigned>", v, &mut stmts);
                let read = self.mk(H::Local(l, hir::UseMode::Copy), f64_, span);
                let stored = self.mk(H::Cast(Box::new(read)), lty, span);
                let unit = self.cx.ty.unit;
                let kind = H::Assign {
                    place: Box::new(place),
                    value: Box::new(stored),
                };
                let assign = self.mk(kind, unit, span);
                stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Expr(assign),
                    span,
                });
                l
            }
            Some(op)
                if !matches!(
                    op,
                    ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish
                ) =>
            {
                let v = self.expr(value, Some(f64_), crate::body::Want::Borrow);
                if self.cx.dynamic(v.ty).is_some() {
                    return Ok(self.any_as_storage(lty, span));
                }
                let Some((assign, Some(l))) =
                    self.storage_rmw_kept(place, op, v, &mut stmts, true, span)
                else {
                    return Ok(self.error_expr(span));
                };
                stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Expr(assign),
                    span,
                });
                l
            }
            _ => return Err(place),
        };
        let value = self.mk(H::Local(kept, hir::UseMode::Copy), f64_, span);
        Ok(self.with_temps_value(stmts, value))
    }

    /// `w ||= v` (also `&&=`, `??=`) on an `f16`/`bf16` place without effects, used as a value:
    /// `w || (w = v)` as JS defines it, so the value is the `number` read from `w`, or `v`
    /// before the store rounds it (#2049).
    pub(super) fn storage_logical_value(
        &mut self,
        op: ast::BinaryOp,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let synthetic = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let assign = synthetic(ast::ExprKind::Assign {
            op: None,
            target: Box::new(target.clone()),
            value: Box::new(value.clone()),
        });
        let either = synthetic(ast::ExprKind::Binary {
            op,
            lhs: Box::new(target.clone()),
            rhs: Box::new(synthetic(ast::ExprKind::Paren(Box::new(assign)))),
        });
        self.expr(&either, None, crate::body::Want::Borrow)
    }

    /// The search and key methods of a collection of `f16`/`bf16` values (`includes`,
    /// `indexOf`, `lastIndexOf` of an array, the `(f16 | null)[]` forms too; `get`, `has`,
    /// `delete` of a `Map` key or a `Set` value) with a `number` argument compare it exactly, as
    /// the operators do (#2049): `xs.includes(0.1)` is false, as in JS with a `Float16Array`.
    /// Written as `x != x || (x as f16) == x ? recv.m(x as f16, …) : <not found>`, the receiver
    /// and then the argument evaluated once, in order. `Err(recv)` for any other call.
    pub(super) fn storage_search(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let key = match prop.name.as_str() {
            "includes" | "indexOf" | "lastIndexOf" => self.cx.ty.array_elem(recv.ty),
            "get" | "has" | "delete" => match self.cx.ty.kind(recv.ty).clone() {
                TyKind::Adt(d, a) if Some(d) == self.cx.prelude_adt("Map") => a.first().copied(),
                TyKind::Adt(d, a) if Some(d) == self.cx.prelude_adt("Set") => a.first().copied(),
                _ => None,
            },
            _ => None,
        };
        let key = key.map(|k| self.cx.ty.core(k).unwrap_or(k));
        let Some(key) = key.filter(|k| self.cx.ty.is_storage_float(*k)) else {
            return Err(recv);
        };
        let Some((first, rest)) = args.split_first() else {
            return Err(recv);
        };
        // The call this writes (its argument a synthetic temporary, which no program names).
        let synthetic =
            |e: &ast::Expr| matches!(&e.kind, ast::ExprKind::Ident(i) if i.name.starts_with('<'));
        let inner = match &first.kind {
            ast::ExprKind::Cast { expr, .. } => synthetic(expr),
            _ => synthetic(first),
        };
        // `null`/`undefined` (an `(f16 | null)[]`'s) take the method as it is.
        let nothing = matches!(
            &first.kind,
            ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
        );
        if inner || nothing {
            return Err(recv);
        }
        let mut stmts = Vec::new();
        // The receiver first, then the argument, as in JS: a place whose evaluation changes
        // nothing reads in place; anything else (`arrs[f()]`) is evaluated into a temporary
        // here, before the argument, even where the argument isn't held.
        let pure =
            crate::body::places::is_place(&recv) && !crate::effects::may_change_memory(&recv);
        let recv = match pure {
            true => recv,
            false => {
                let id = self.temp_ident("<recv>", recv, &mut stmts);
                self.expr(&id, None, crate::body::Want::Borrow)
            }
        };
        let a = self.expr(first, None, crate::body::Want::Borrow);
        let number = self.cx.ty.is_numeric(a.ty) && a.ty != key;
        if !number {
            // The element type itself (or a type the call reports): the method as it is.
            let id = self.temp_ident("<key>", a, &mut stmts);
            let call_args: Vec<ast::Expr> =
                std::iter::once(id).chain(rest.iter().cloned()).collect();
            let call = self.method_call_on(recv, prop, type_args, &call_args, exp, span);
            return Ok(self.with_temps_value(stmts, call));
        }
        let f64_ = self.cx.ty.f64;
        let a = self.coerce(a, f64_);
        let x = self.temp_ident("<number>", a, &mut stmts);
        let tn = self.cx.display(key);
        let synth = super::callback::synth;
        let key_ty = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident { name: tn, span }],
                args: vec![],
            },
            span,
        };
        let as_key = |x: &ast::Expr| {
            let kind = ast::ExprKind::Cast {
                expr: Box::new(x.clone()),
                ty: key_ty.clone(),
            };
            synth(kind, span)
        };
        let binary = |op, lhs: ast::Expr, rhs: ast::Expr| {
            let kind = ast::ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
            synth(kind, span)
        };
        let ne = binary(ast::BinaryOp::NotEq, x.clone(), x.clone());
        let eq = binary(ast::BinaryOp::Eq, as_key(&x), x.clone());
        let held = binary(ast::BinaryOp::Or, ne, eq);
        let cond = self.cond(&held);
        let call_args: Vec<ast::Expr> = std::iter::once(as_key(&x))
            .chain(rest.iter().cloned())
            .collect();
        let call = self.method_call_on(recv, prop, type_args, &call_args, exp, span);
        let miss = match self.cx.ty.kind(call.ty) {
            TyKind::Bool => self.mk(H::Lit(hir::Lit::Bool(false)), call.ty, span),
            TyKind::Int(_) | TyKind::Float(_) => {
                let one = self.mk(H::Lit(hir::Lit::Int(1)), call.ty, span);
                let kind = H::Unary {
                    op: hir::UnOp::Neg,
                    expr: Box::new(one),
                };
                self.mk(kind, call.ty, span)
            }
            TyKind::Option(..) => self.mk(H::Lit(hir::Lit::Null), call.ty, span),
            _ => return Ok(self.with_temps_value(stmts, call)),
        };
        let ty = call.ty;
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(call),
            els: Box::new(miss),
        };
        let h = self.mk(kind, ty, span);
        Ok(self.with_temps_value(stmts, h))
    }

    /// The type a literal beside an `f16`/`bf16` takes in `operands`: `number`, as every
    /// operator reads the storage float as one (#2049).
    pub(super) fn storage_literal_hint(&mut self) -> Option<TyId> {
        Some(self.cx.ty.f64)
    }

    /// `++w`/`w--` on an `f16`/`bf16` place used as a value (#2049): the old value read as a
    /// `number`, the new one `old ± 1` in f64 stored rounded once, and the expression's value
    /// the new number (prefix) or the old one (postfix), as JS gives for a `Float16Array`
    /// element. The place's object and indices are evaluated once.
    pub(super) fn storage_update_value(
        &mut self,
        place: hir::Expr,
        op: ast::BinaryOp,
        prefix: bool,
        span: Span,
    ) -> hir::Expr {
        let (lty, f64_) = (place.ty, self.cx.ty.f64);
        let mut place = place;
        let mut stmts = Vec::new();
        self.hoist_indices(&mut place, &mut stmts);
        let read = self.place_read(&place, crate::body::Want::Borrow);
        let old = self.storage_widened(read);
        let old = self.bind_temp("<old>", old, &mut stmts);
        let get = |s: &mut Self, l| s.mk(H::Local(l, hir::UseMode::Copy), f64_, span);
        let one = self.mk(H::Lit(hir::Lit::Float(1.0)), f64_, span);
        let bop = super::ops::hir_binop(op).expect("ICE: ++ is + or -");
        let lhs = get(self, old);
        let kind = H::Binary {
            op: bop,
            lhs: Box::new(lhs),
            rhs: Box::new(one),
        };
        let new = self.mk(kind, f64_, span);
        let new = self.bind_temp("<new>", new, &mut stmts);
        let stored = get(self, new);
        let stored = self.mk(H::Cast(Box::new(stored)), lty, span);
        let unit = self.cx.ty.unit;
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(stored),
        };
        let assign = self.mk(kind, unit, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(assign),
            span,
        });
        let value = get(self, if prefix { new } else { old });
        self.with_temps_value(stmts, value)
    }
}

/// The value of a number literal without a suffix, possibly negated or in parentheses: what
/// `w = L` used as a value yields unrounded.
fn plain_literal(e: &ast::Expr) -> Option<f64> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Int {
            value,
            suffix: None,
        }) => Some(*value as f64),
        ast::ExprKind::Lit(ast::Lit::Float {
            value,
            suffix: None,
        }) => Some(*value),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => plain_literal(expr).map(|v| -v),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Plus,
            expr,
        }
        | ast::ExprKind::Paren(expr) => plain_literal(expr),
        _ => None,
    }
}
