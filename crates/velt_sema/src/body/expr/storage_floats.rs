//! The storage floats `f16` and `bf16` in expressions (#2049): arithmetic on them runs in
//! `f32` (every operator widens a storage operand first), a literal of one is its f64 value
//! rounded once (an error where that is infinite), and only `as` narrows into one.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` widened to `f32` if it is an `f16` or `bf16` (exact), the type arithmetic on it runs
    /// in (#2049); `h` as it is otherwise.
    pub(crate) fn storage_widened(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.cx.ty.is_storage_float(h.ty) {
            return h;
        }
        let f32_ = self
            .cx
            .ty
            .primitive("f32")
            .expect("ICE: f32 is a primitive");
        let span = h.span;
        self.mk(H::Cast(Box::new(h)), f32_, span)
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

    /// A bitwise operator with an `f16`/`bf16` operand: an error (#2049), reported before
    /// the JS ToInt32 path that `number` operands take.
    pub(super) fn storage_bitwise(
        &mut self,
        op: ast::BinaryOp,
        l: &hir::Expr,
        r: &hir::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        use ast::BinaryOp as B;
        let bitwise = matches!(
            op,
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr | B::UShr
        );
        let h = [l, r]
            .into_iter()
            .find(|h| bitwise && self.cx.ty.is_storage_float(h.ty))?;
        let t = self.cx.display(h.ty);
        self.cx.error(
            Diagnostic::error(format!("bitwise operators take integers, not `{t}`"), span)
                .with_note(format!(
                    "an `{t}` is a float: convert it to an integer type with `as` first"
                )),
        );
        Some(self.error_expr(span))
    }

    /// `place op= v` (or `place++`, `v` the `f32` 1) on an `f16`/`bf16` place (#2049): the place
    /// is read, both sides widen to `f32`, the operation runs there and its result rounds back
    /// once. The place's object and indices are evaluated once, before `v`. `v` is an `f16`/
    /// `bf16` of the place's own type or a literal (typed as it, as `=` types one); a wider value
    /// needs the cast, as for `=`. `None` after reporting.
    pub(super) fn storage_rmw(
        &mut self,
        place: hir::Expr,
        op: hir::BinOp,
        v: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> Option<hir::Expr> {
        let lty = place.ty;
        let f32_ = self
            .cx
            .ty
            .primitive("f32")
            .expect("ICE: f32 is a primitive");
        // A literal on the right is typed as the place (`=`'s rule), so only the place's type
        // passes; an `f32` (`0.1f32` too) needs the cast.
        if v.ty != lty && !self.cx.ty.is_bottom(v.ty) {
            let (t, vt) = (self.cx.display(lty), self.cx.display(v.ty));
            self.cx.error(
                Diagnostic::error(
                    format!("an `{vt}` doesn't convert to `{t}` implicitly"),
                    span,
                )
                .with_note(format!(
                    "compound assignment on an `{t}` takes an `{t}` or a literal: write `x = (x op v) as {t}` (rounds once)"
                )),
            );
            return None;
        }
        let mut place = place;
        self.hoist_indices(&mut place, stmts);
        // As the number path: the object a narrowed field held before `v` (#1280).
        self.hoist_checked_object(&mut place, &v, stmts);
        let cur = self.place_read(&place, crate::body::Want::Borrow);
        let (l, r) = (self.storage_widened(cur), self.storage_widened(v));
        let wide = match op {
            hir::BinOp::Div => self.divide(l, r, f32_, span),
            _ => {
                let kind = H::Binary {
                    op,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                };
                self.mk(kind, f32_, span)
            }
        };
        let narrowed = self.mk(H::Cast(Box::new(wide)), lty, span);
        // A held tuple `v` cleared is reported as on the number path (#1352).
        let narrowed = self.held_rechecked(narrowed);
        let unit = self.cx.ty.unit;
        let kind = H::Assign {
            place: Box::new(place),
            value: Box::new(narrowed),
        };
        Some(self.mk(kind, unit, span))
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

    /// The type a literal beside an `f16`/`bf16` takes in `operands`: `f32`, the type arithmetic
    /// on it runs in, or `f64` in a comparison (`comparing`), which is then exact (#2049): `w ==
    /// 0.1` compares as JS compares a `Float16Array` element with the number.
    pub(super) fn storage_literal_hint(&mut self) -> Option<TyId> {
        match self.comparing {
            true => Some(self.cx.ty.f64),
            false => self.cx.ty.primitive("f32"),
        }
    }

    /// `f` with `comparing` set to `on` (a comparison's operands), restored after.
    pub(super) fn comparing_in<R>(&mut self, on: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let outer = std::mem::replace(&mut self.comparing, on);
        let r = f(self);
        self.comparing = outer;
        r
    }
}
