//! `s as Route` for a `string` `s` and a union of string literals `Route`, used as a comparison
//! (#1410). TypeScript accepts the assertion and keeps the string, so `ROUTES.includes(path as
//! Route)` tests a string that may be none of the literals, and Node answers `false`. As the
//! argument of `includes` / `indexOf` / `lastIndexOf` on an array, of `has` on a `Set`, of
//! `has` / `get` on a `Map`, as an operand of `===` / `!==` / `==` / `!=`, and as a `switch`
//! discriminant, the assertion only compares the string: an unknown value is not found
//! (`false` / `-1` / `undefined`), unequal, or `default`'s, and nothing throws. Anywhere else
//! it is the checked conversion of `literal_union_cast`, which throws a `TypeError` on a miss.

use velt_common::Span;
use velt_syntax::ast;

use super::callback::synth;
use super::method_value::is_path;
use super::object_builtins::strip_parens;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

/// A temporary this module (or another rewrite) wrote, which no program names.
fn synthetic(e: &ast::Expr) -> bool {
    matches!(&strip_parens(e).kind, ast::ExprKind::Ident(i) if i.name.starts_with('<'))
}

/// How the asserted value is known to be a `string`: a path's type is peeked; another value
/// (a call, an index read, …) is typed where it is evaluated, and asserted as it is when it
/// turns out not to be one.
enum Source {
    Path,
    Value,
}

impl FnCx<'_, '_> {
    /// `(inner as T)` with `T` a union of string literals (no `null`): `inner`, the written
    /// type and `T`'s literals, when `inner` may be a `string`.
    fn string_as_literals<'e>(
        &mut self,
        e: &'e ast::Expr,
    ) -> Option<(&'e ast::Expr, &'e ast::TypeExpr, Vec<String>, Source)> {
        let ast::ExprKind::Cast { expr, ty } = &strip_parens(e).kind else {
            return None;
        };
        if super::member::is_as_const(ty) || synthetic(expr) {
            return None;
        }
        let source = match &strip_parens(expr).kind {
            _ if is_path(expr) => Source::Path,
            // A literal, or a value its context shapes, takes the assertion as it is.
            ast::ExprKind::Lit(_)
            | ast::ExprKind::Cond { .. }
            | ast::ExprKind::Arrow { .. }
            | ast::ExprKind::Function(_)
            | ast::ExprKind::Array(_)
            | ast::ExprKind::NumArray(_)
            | ast::ExprKind::Object(_)
            | ast::ExprKind::StructLit { .. }
            | ast::ExprKind::Jsx(_) => return None,
            _ => Source::Value,
        };
        // Resolved again (with its errors) where the assertion is checked.
        let mark = self.cx.diags.len();
        let target = self.resolve(ty);
        self.cx.diags.truncate(mark);
        if self.cx.ty.core(target).is_some() {
            return None;
        }
        let values = self.string_literals(target)?;
        if matches!(source, Source::Path) && self.peek_ty(expr) != Some(self.cx.ty.str_) {
            return None;
        }
        Some((expr, ty, values, source))
    }

    /// `s` for a `switch (s as Route)` discriminant of a `string` `s` (module docs): the cases
    /// compare the string, and one that matches no case goes to `default`, as in Node.
    pub(crate) fn switch_string_operand(&mut self, disc: &ast::Expr) -> Option<ast::Expr> {
        let (inner, _, _, source) = self.string_as_literals(disc)?;
        let string = match source {
            Source::Path => true,
            Source::Value => self.trial_type(inner) == Some(self.cx.ty.str_),
        };
        string.then(|| inner.clone())
    }

    /// `s === "a" || s === "b" || …` over `values`, as a condition.
    fn literal_member_test(&mut self, s: &ast::Expr, values: &[String], span: Span) -> hir::Expr {
        let eq = |v: &String| {
            let lit = synth(ast::ExprKind::Lit(ast::Lit::Str(v.clone())), span);
            let kind = ast::ExprKind::Binary {
                op: ast::BinaryOp::StrictEq,
                lhs: Box::new(s.clone()),
                rhs: Box::new(lit),
            };
            synth(kind, span)
        };
        let test = (values.iter().map(eq))
            .reduce(|a, b| {
                let kind = ast::ExprKind::Binary {
                    op: ast::BinaryOp::Or,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                };
                synth(kind, span)
            })
            .unwrap_or_else(|| synth(ast::ExprKind::Lit(ast::Lit::Bool(false)), span));
        self.cond(&test)
    }

    /// The key type a search or key method compares its first argument with: an array's
    /// element for `includes` / `indexOf` / `lastIndexOf`, a `Set`'s value for `has`, a
    /// `Map`'s key for `has` / `get`.
    fn search_key(&mut self, recv: TyId, method: &str) -> Option<TyId> {
        let key = match method {
            "includes" | "indexOf" | "lastIndexOf" => self.cx.ty.array_elem(recv),
            "has" | "get" => match self.cx.ty.kind(recv).clone() {
                TyKind::Adt(d, a) if Some(d) == self.cx.prelude_adt("Map") => a.first().copied(),
                TyKind::Adt(d, a) if Some(d) == self.cx.prelude_adt("Set") && method == "has" => {
                    a.first().copied()
                }
                _ => None,
            },
            _ => None,
        }?;
        Some(self.cx.ty.core(key).unwrap_or(key))
    }

    /// `xs.includes(s as Route)`, `indexOf` / `lastIndexOf`, `set.has(s as Route)`,
    /// `map.has(s as Route)` / `map.get(s as Route)` (module docs). Against keys of the literal
    /// union, `s` is tested against the literals first, and a miss is `false` / `-1` /
    /// `undefined` without converting; against `string` keys (a `string[]`, an array literal),
    /// `s` is passed as it is. The receiver is evaluated before the argument, as in JS.
    /// `Err(recv)` for any other call.
    pub(super) fn literal_search(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(key) = self.search_key(recv.ty, &prop.name) else {
            return Err(recv);
        };
        let plain_key = key == self.cx.ty.str_;
        if !plain_key && self.string_literals(key).is_none() {
            return Err(recv);
        }
        let Some((first, rest)) = args.split_first() else {
            return Err(recv);
        };
        let Some((inner, ty, values, _)) = self.string_as_literals(first) else {
            return Err(recv);
        };
        let mut stmts = Vec::new();
        let pure =
            crate::body::places::is_place(&recv) && !crate::effects::may_change_memory(&recv);
        let recv = match pure {
            true => recv,
            false => {
                let id = self.temp_ident("<recv>", recv, &mut stmts);
                self.expr(&id, None, Want::Borrow)
            }
        };
        let a = self.expr(inner, None, Want::Borrow);
        let string = a.ty == self.cx.ty.str_;
        let s = self.temp_ident("<str>", a, &mut stmts);
        let arg = match string && plain_key {
            true => s.clone(),
            false => synth(
                ast::ExprKind::Cast {
                    expr: Box::new(s.clone()),
                    ty: ty.clone(),
                },
                span,
            ),
        };
        let call_args: Vec<ast::Expr> = std::iter::once(arg).chain(rest.iter().cloned()).collect();
        if !string || plain_key {
            let call = self.method_call_on(recv, prop, type_args, &call_args, exp, span);
            return Ok(self.with_temps_value(stmts, call));
        }
        let cond = self.literal_member_test(&s, &values, span);
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

    /// `x === (s as Route)` (either side, and `!==` / `==` / `!=`; module docs): the string is
    /// compared as it is, as in Node, so an unknown value is unequal. Operands are evaluated
    /// once, left to right. `None` for other comparisons.
    pub(super) fn literal_cast_compare(
        &mut self,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let (cast_left, (inner, ty, values, source)) = match self.string_as_literals(lhs) {
            Some(c) => (true, c),
            None => (false, self.string_as_literals(rhs)?),
        };
        // A path is a `string` (peeked): compared without the assertion, where the other
        // operand compares with a `string`.
        if matches!(source, Source::Path) {
            let (l, r) = match cast_left {
                true => (inner, rhs),
                false => (lhs, inner),
            };
            if self
                .trial_with(|cx| cx.binary(op, l, r, exp, span).ty)
                .is_some()
            {
                return Some(self.binary(op, l, r, exp, span));
            }
        }
        // Otherwise the operands are evaluated in order, each into a temporary unless it is a
        // path or a literal, and the string is compared as it is; where the other operand
        // doesn't compare with a `string` (a `Route | null`), the string is tested against the
        // literals first, and a miss is unequal.
        let mut stmts = Vec::new();
        let plain =
            |e: &ast::Expr| is_path(e) || matches!(strip_parens(e).kind, ast::ExprKind::Lit(_));
        // The other operand; a `string` asserted to literals there compares as it is too
        // (`(a() as Route) === (b() as Route)`).
        let other = |cx: &mut Self, e: &ast::Expr, stmts: &mut Vec<hir::Stmt>| {
            if let Some((inner, ty, _, _)) = cx.string_as_literals(e) {
                let h = cx.expr(inner, None, Want::Borrow);
                let string = h.ty == cx.cx.ty.str_;
                let t = cx.temp_ident("<operand>", h, stmts);
                return match string {
                    true => t,
                    false => synth(
                        ast::ExprKind::Cast {
                            expr: Box::new(t),
                            ty: ty.clone(),
                        },
                        e.span,
                    ),
                };
            }
            match plain(e) {
                true => e.clone(),
                false => {
                    let h = cx.expr(e, None, Want::Borrow);
                    cx.temp_ident("<operand>", h, stmts)
                }
            }
        };
        let left = (!cast_left).then(|| other(self, lhs, &mut stmts));
        let a = self.expr(inner, None, Want::Borrow);
        let string = a.ty == self.cx.ty.str_;
        let s = self.temp_ident("<str>", a, &mut stmts);
        let right = cast_left.then(|| other(self, rhs, &mut stmts));
        let cast = synth(
            ast::ExprKind::Cast {
                expr: Box::new(s.clone()),
                ty: ty.clone(),
            },
            span,
        );
        let pair = |x: ast::Expr| match (&left, &right) {
            (Some(l), _) => (l.clone(), x),
            (_, Some(r)) => (x, r.clone()),
            _ => unreachable!("ICE: a comparison has two operands"),
        };
        if !string {
            let (l, r) = pair(cast);
            let h = self.binary(op, &l, &r, exp, span);
            return Some(self.with_temps_value(stmts, h));
        }
        let (l, r) = pair(s.clone());
        if self
            .trial_with(|cx| cx.binary(op, &l, &r, exp, span).ty)
            .is_some()
        {
            let h = self.binary(op, &l, &r, exp, span);
            return Some(self.with_temps_value(stmts, h));
        }
        let cond = self.literal_member_test(&s, &values, span);
        let (l, r) = pair(cast);
        let hit = self.binary(op, &l, &r, exp, span);
        let negated = matches!(op, ast::BinaryOp::NotEq | ast::BinaryOp::StrictNotEq);
        let miss = self.mk(H::Lit(hir::Lit::Bool(negated)), hit.ty, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(hit),
            els: Box::new(miss),
        };
        let h = self.mk(kind, self.cx.ty.bool_, span);
        Some(self.with_temps_value(stmts, h))
    }
}
