//! A scalar operand of a vector operator (design operators.md, #2050): `v * 2`, `2 * v` and
//! `v + bias` with `v` a `velt/simd` vector are the operator on `v` and the scalar splatted to
//! every lane (`v * f32x4.splat(2)`), when the vector's operator method takes the vector type
//! (a shift takes its `u32` count as it is). A literal takes the lane type; a variable must
//! already have it (`f32x4 * x` with `x: f64` is an error: `x as f32`), the narrowing rule of
//! scalars.
//!
//! ```text
//! v * k        { const <scalar> = k; v[Symbol.mul](f32x4.splat(<scalar>)) }
//! 2 * v        f32x4.splat(2)[Symbol.mul](v)
//! ```

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::method::Resolved;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, TyId, TyKind};
use velt_syntax::operators;

/// The temporary holding a scalar operand that is not a literal.
const SCALAR: &str = "<scalar>";
/// The temporary holding the vector operand when it has to be evaluated before the scalar.
const VECTOR: &str = "<vector>";
/// The temporary holding the splatted scalar.
const SPLAT: &str = "<splat>";

fn name_ref(name: &str, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Ident(ast::Ident {
            name: name.into(),
            span,
        }),
        span,
    )
}

impl FnCx<'_, '_> {
    /// The static `splat(x)` of vector type `t` and the lane type it takes.
    fn splat_of(&self, t: TyId) -> Option<(DefId, TyId)> {
        let def = self.cx.extensions.iter().find_map(|x| {
            let m = x.methods.get("splat")?;
            (x.target == t && m.is_static).then_some(m.def)
        })?;
        let lane = self.cx.fn_info(def).params.first()?.ty;
        Some((def, lane))
    }

    /// Does vector type `t`'s operator method `name` take a vector of its own type?
    fn takes_vector(&mut self, t: TyId, name: &str) -> bool {
        let def = match self.resolve_method(t, name) {
            Some(Resolved::Def { def, .. }) => def,
            _ => return false,
        };
        self.cx
            .fn_info(def)
            .params
            .first()
            .is_some_and(|p| p.ty == t)
    }

    /// `l op r` with one operand a vector and the other a scalar for it: the scalar splatted;
    /// `Err` gives the operands back when that is not the case.
    #[allow(clippy::too_many_arguments)] // both operands as written and checked
    pub(super) fn simd_scalar_operator(
        &mut self,
        op: ast::BinaryOp,
        (lhs, rhs): (&ast::Expr, &ast::Expr),
        l: hir::Expr,
        r: hir::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, Box<(hir::Expr, hir::Expr)>> {
        let Some(name) = operators::binary_method(op) else {
            return Err(Box::new((l, r)));
        };
        let simd = |s: &Self, t: TyId| matches!(s.cx.ty.kind(t), TyKind::Simd(_));
        let vector_left = match (simd(self, l.ty), simd(self, r.ty)) {
            (true, false) => true,
            (false, true) => false,
            _ => return Err(Box::new((l, r))),
        };
        let vt = if vector_left { l.ty } else { r.ty };
        let Some((splat, lane)) = self.splat_of(vt) else {
            return Err(Box::new((l, r)));
        };
        if !self.takes_vector(vt, name) {
            return Err(Box::new((l, r)));
        }
        let (scalar_ast, scalar) = if vector_left { (rhs, &r) } else { (lhs, &l) };
        let literal = super::ops::untyped(scalar_ast);
        if literal {
            // Checked as a constant of the lane type (`i32x4 * 2.5` is an error, as
            // `const k: i32 = 2.5` is), not converted as a number passed to std.
            let before = self.cx.diags.len();
            let h = self.expr(scalar_ast, Some(lane), Want::Move);
            self.coerce(h, lane);
            if self.cx.diags.len() > before {
                return Ok(self.error_expr(span));
            }
        }
        if !literal && scalar.ty != lane && !self.cx.ty.is_bottom(scalar.ty) {
            let (sn, ln, vn) = (
                self.cx.display(scalar.ty),
                self.cx.display(lane),
                self.cx.display(vt),
            );
            self.cx.error(
                Diagnostic::error(
                    format!("a scalar operand of `{vn}` must be a `{ln}`, not a `{sn}`"),
                    scalar_ast.span,
                )
                .with_note(format!("convert it first: `x as {ln}`")),
            );
            return Ok(self.error_expr(span));
        }
        let prop = ast::Ident {
            name: name.into(),
            span,
        };
        let mut stmts = vec![];
        self.push_scope();
        let call = if vector_left {
            // `v op k`: the vector first (held unless the scalar is a literal: `obj.v + f()`
            // reads `obj.v` before `f` runs), then the scalar, splatted, as the argument.
            let recv = if literal && super::accessor_rmw::is_field_path(&l) {
                l
            } else {
                let (ty, at) = (l.ty, l.span);
                let held = self.bind_temp(VECTOR, l, &mut stmts);
                self.mk(hir::ExprKind::Local(held, hir::UseMode::Copy), ty, at)
            };
            let arg = match literal {
                true => rhs.clone(),
                false => {
                    let at = r.span;
                    self.bind_temp(SCALAR, r, &mut stmts);
                    name_ref(SCALAR, at)
                }
            };
            let splatted = self.splat_call(splat, &arg, span);
            self.bind_temp(SPLAT, splatted, &mut stmts);
            self.method_call_on(recv, &prop, &[], &[name_ref(SPLAT, span)], exp, span)
        } else {
            // `k op v`: the scalar splatted first, then the vector as the argument.
            let recv = match literal {
                true => self.splat_call(splat, lhs, span),
                false => {
                    let at = l.span;
                    self.bind_temp(SCALAR, l, &mut stmts);
                    self.splat_call(splat, &name_ref(SCALAR, at), span)
                }
            };
            let at = r.span;
            let r = self.share_if_place(r);
            self.bind_temp(VECTOR, r, &mut stmts);
            self.method_call_on(recv, &prop, &[], &[name_ref(VECTOR, at)], exp, span)
        };
        self.pop_scope();
        Ok(self.with_temps_value(stmts, call))
    }

    /// `T.splat(arg)`.
    fn splat_call(&mut self, splat: DefId, arg: &ast::Expr, span: Span) -> hir::Expr {
        let c = self.fn_callable(splat, "`splat`".to_string(), span);
        let slots = vec![None; c.slot_names.len()];
        let ck = self.check_call(&c, slots, std::slice::from_ref(arg), None, span);
        self.def_call(splat, ck, span)
    }
}
