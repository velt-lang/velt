//! Operator methods (design operators.md, #2050). An operator whose operand types have no
//! built-in meaning for it calls a method keyed by a well-known symbol (`velt_syntax::operators`):
//!
//! - `a op b` is `a[Symbol.op](b)` when `A` has that instance method, else `B[Symbol.op](a, b)`
//!   when `B` has a static one (the form for a left operand of another type, `2 * v`). Both
//!   applicable is an ambiguity error, so a static added to `B` never changes what an existing
//!   `a op b` calls.
//! - `-a` / `~a` is `a[Symbol.neg]()` / `a[Symbol.bitNot]()`.
//!
//! The call is checked as the method call written out (overloads, generics, `throws`, receiver
//! modes, `extend` blocks, interface values and bounds of a type parameter), so an operator
//! costs nothing over that call. `+` with a `string` operand is always concatenation, and an
//! `any` or `unknown` operand keeps JS's operators: neither reaches this module.
//!
//! The operands are evaluated left to right. A literal, a variable or field path, or an element
//! of one at a pure index is passed as written (checked again as the argument); any other
//! operand is evaluated once into a temporary, the left one first:
//!
//! ```text
//! a + f()      { const <rhs> = f(); a[Symbol.add](<rhs>) }
//! g() + f()    { const <lhs> = g(); const <rhs> = f(); <lhs>[Symbol.add](<rhs>) }
//! 2 * v        Vec[Symbol.mul](2, v)
//! ```

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::assign_paths::is_pure_place;
use super::ops::op_str;
use super::setters::synth;
use crate::body::FnCx;
use crate::defs::MethodRef;
use crate::hir::{self, DefId, ExprKind as H, TyId, TyKind, UseMode};
use velt_syntax::operators;

/// The temporary holding a left operand evaluated before a right one with effects.
const LHS: &str = "<lhs>";
/// The temporary holding a right operand that is not passed as written.
const RHS: &str = "<rhs>";

/// Where the static form of an operator is declared.
#[derive(Clone, Copy)]
enum StaticOp {
    /// A static method of class or struct `d` (own, inherited or from an `extend` block).
    Adt(DefId),
    /// A static method of an `extend` block of a type that is not a class (a vector type).
    Ext(DefId),
}

/// Is `e` passed to an operator method as written: a literal (a negated number too), or a
/// variable, field path or element of one at a pure index (`b[i]`), which checking again gives
/// the same value, evaluated once?
fn as_written(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Paren(inner) => as_written(inner),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => matches!(expr.kind, ast::ExprKind::Lit(_)),
        _ => is_pure_place(e),
    }
}

/// The span naming the operator of `lhs op rhs`: between the operands (hover and
/// go-to-definition find the method there).
fn op_span(lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> Span {
    if lhs.span.file == rhs.span.file && lhs.span.hi <= rhs.span.lo {
        Span {
            lo: lhs.span.hi,
            hi: rhs.span.lo,
            ..lhs.span
        }
    } else {
        span
    }
}

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
    /// Can values of type `t` have operator methods: a class, struct, vector, interface value or
    /// type parameter (shared or not)? The other types have built-in operators or none.
    pub(crate) fn operator_type(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => {
                self.record_args(t).is_none()
                    && self
                        .cx
                        .adt(*d)
                        .is_some_and(|a| a.kind != hir::AdtKind::Anon)
            }
            TyKind::Param(_) | TyKind::Simd(_) | TyKind::Dyn(..) => true,
            TyKind::Shared(inner) => self.operator_type(*inner),
            _ => false,
        }
    }

    /// May `l op r` call an operator method: `op` has one, and an operand's type may declare it
    /// (not `+` with a string: `string`, a string literal type or a union of them)? A test by reference, so the operands of the built-in operators
    /// pass by without a move.
    pub(super) fn may_call_operator(
        &self,
        op: ast::BinaryOp,
        l: &hir::Expr,
        r: &hir::Expr,
    ) -> bool {
        operators::binary_method(op).is_some()
            && !(op == ast::BinaryOp::Add
                && (self.cx.string_like(l.ty) || self.cx.string_like(r.ty)))
            && (self.operator_type(l.ty) || self.operator_type(r.ty))
    }

    /// Does `t` have the instance operator method `name`?
    pub(crate) fn has_instance_op(&mut self, t: TyId, name: &str) -> bool {
        if !self.operator_type(t) {
            return false;
        }
        match self.resolve_method(t, name) {
            Some(super::method::Resolved::Def { is_static, .. }) => !is_static,
            Some(_) => true,
            None => false,
        }
    }

    /// The static operator method `name` of `t`, if it has one.
    fn static_op(&mut self, t: TyId, name: &str) -> Option<StaticOp> {
        if !self.operator_type(t) {
            return None;
        }
        if let TyKind::Adt(d, _) = *self.cx.ty.kind(t) {
            return self
                .find_static(d, name)
                .is_ok()
                .then_some(StaticOp::Adt(d));
        }
        let key = crate::defs::static_key(name);
        let ext = self.cx.extensions.iter().find_map(|x| {
            let m: &MethodRef = x.methods.get(&key).or_else(|| x.methods.get(name))?;
            (x.target == t && m.is_static).then_some(m.def)
        });
        ext.map(StaticOp::Ext)
    }

    /// `lhs op rhs` through an operator method; `Err` gives the checked operands back when the
    /// operator has its built-in meaning (or none, reported by the caller).
    #[allow(clippy::too_many_arguments)] // both operands as written and checked
    pub(super) fn binary_operator(
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
        if op == ast::BinaryOp::Add && (self.cx.string_like(l.ty) || self.cx.string_like(r.ty)) {
            return Err(Box::new((l, r)));
        }
        if !self.operator_type(l.ty) && !self.operator_type(r.ty) {
            return Err(Box::new((l, r)));
        }
        // A vector and a scalar for it: the scalar splatted (`operator_simd.rs`).
        let (l, r) = match self.simd_scalar_operator(op, (lhs, rhs), l, r, exp, span) {
            Ok(h) => return Ok(h),
            Err(lr) => *lr,
        };
        let prop = ast::Ident {
            name: name.into(),
            span: op_span(lhs, rhs, span),
        };
        let left = self.has_instance_op(l.ty, name);
        let right = self.static_op(r.ty, name);
        match (left, right) {
            (false, None) => {
                self.no_operator(op, &l, &r, span);
                Ok(self.error_expr(span))
            }
            (true, None) => Ok(self.left_operator_call(&prop, (lhs, rhs), l, r, exp, span)),
            (false, Some(st)) => Ok(self.static_operator_call(st, &prop, (lhs, rhs), l, r, exp)),
            (true, Some(st)) => {
                let by_left = self.attempt(|s| {
                    s.method_call_on(l.clone(), &prop, &[], std::slice::from_ref(rhs), exp, span);
                });
                let by_right = self.attempt(|s| {
                    s.static_op_call(st, &prop, &[lhs.clone(), rhs.clone()], exp, span);
                });
                match (by_left, by_right) {
                    (true, true) => {
                        self.ambiguous_operator(op, &l, &r, span);
                        Ok(self.error_expr(span))
                    }
                    (false, true) => {
                        Ok(self.static_operator_call(st, &prop, (lhs, rhs), l, r, exp))
                    }
                    // Neither accepts: the instance method's own errors.
                    _ => Ok(self.left_operator_call(&prop, (lhs, rhs), l, r, exp, span)),
                }
            }
        }
    }

    /// Does the check `f` pass without errors? What it added is undone either way.
    fn attempt(&mut self, f: impl FnOnce(&mut Self)) -> bool {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let temps = self.call_temps.len();
        f(self);
        let ok = !self.cx.diags[diags..].iter().any(|d| d.is_error());
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.call_temps.truncate(temps);
        self.restore_trial_frames(frames);
        ok
    }

    /// `l[Symbol.op](r)`.
    fn left_operator_call(
        &mut self,
        prop: &ast::Ident,
        (_, rhs): (&ast::Expr, &ast::Expr),
        l: hir::Expr,
        r: hir::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if as_written(rhs) {
            return self.method_call_on(l, prop, &[], std::slice::from_ref(rhs), exp, span);
        }
        self.push_scope();
        let mut stmts = vec![];
        // The left operand is evaluated first: a path is read where the call reads it, as
        // `xs[i] op= v` reads its receiver (`indexers`).
        let recv = if super::accessor_rmw::is_field_path(&l) {
            l
        } else {
            let (ty, at) = (l.ty, l.span);
            let held = self.bind_temp(LHS, l, &mut stmts);
            self.mk(H::Local(held, UseMode::Borrow), ty, at)
        };
        let r_span = r.span;
        let r = self.share_if_place(r);
        self.bind_temp(RHS, r, &mut stmts);
        let call = self.method_call_on(recv, prop, &[], &[name_ref(RHS, r_span)], exp, span);
        self.pop_scope();
        self.with_temps_value(stmts, call)
    }

    /// `B[Symbol.op](l, r)`, the static form.
    fn static_operator_call(
        &mut self,
        st: StaticOp,
        prop: &ast::Ident,
        (lhs, rhs): (&ast::Expr, &ast::Expr),
        l: hir::Expr,
        r: hir::Expr,
        exp: Option<TyId>,
    ) -> hir::Expr {
        let span = prop.span;
        self.push_scope();
        let mut stmts = vec![];
        let mut arg = |s: &mut Self, e: &ast::Expr, h: hir::Expr, name: &str| {
            if as_written(e) {
                return e.clone();
            }
            let at = h.span;
            let h = s.share_if_place(h);
            s.bind_temp(name, h, &mut stmts);
            name_ref(name, at)
        };
        let args = [arg(self, lhs, l, LHS), arg(self, rhs, r, RHS)];
        let call = self.static_op_call(st, prop, &args, exp, span);
        self.pop_scope();
        self.with_temps_value(stmts, call)
    }

    /// The call of static operator method `st` with `args`.
    fn static_op_call(
        &mut self,
        st: StaticOp,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        match st {
            StaticOp::Adt(d) => self.static_call(d, prop, &[], args, exp, span),
            StaticOp::Ext(def) => {
                let c = self.fn_callable(def, format!("`{}`", prop.name), span);
                let slots = vec![None; c.slot_names.len()];
                let ck = self.check_call(&c, slots, args, exp, span);
                self.def_call(def, ck, span)
            }
        }
    }

    /// `-a` / `~a` through `[Symbol.neg]` / `[Symbol.bitNot]`; `Err` gives the operand back.
    pub(super) fn unary_operator(
        &mut self,
        op: ast::UnaryOp,
        inner: hir::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(name) = operators::unary_method(op) else {
            return Err(inner);
        };
        if !self.has_instance_op(inner.ty, name) {
            return Err(inner);
        }
        let at = Span {
            hi: inner.span.lo.max(span.lo),
            ..span
        };
        let prop = ast::Ident {
            name: name.into(),
            span: at,
        };
        Ok(self.method_call_on(inner, &prop, &[], &[], exp, span))
    }

    /// The operator method a type lacks: ``cannot apply binary operator `*` to type `V` ``.
    fn no_operator(&mut self, op: ast::BinaryOp, l: &hir::Expr, r: &hir::Expr, span: Span) {
        if self.cx.ty.is_bottom(l.ty) || self.cx.ty.is_bottom(r.ty) {
            return;
        }
        let name = operators::binary_method(op).unwrap_or_default();
        let (on, other) = match self.operator_type(l.ty) {
            true => (l.ty, r.ty),
            false => (r.ty, l.ty),
        };
        let (tn, on_left) = (self.cx.display(on), on == l.ty);
        let other_n = self.cx.display(other);
        let message = format!(
            "cannot apply binary operator `{}` to type `{tn}`",
            op_str(op)
        );
        if matches!(self.cx.ty.kind(on), TyKind::Simd(_)) {
            let note = super::ops::simd_method_note(op);
            self.cx
                .error(Diagnostic::error(message, span).with_note(note));
            return;
        }
        let help = match self.cx.ty.kind(on) {
            TyKind::Param(_) => format!(
                "bound the type parameter by `velt/ops`' interface for it, such as `{tn} extends {}<{other_n}>`",
                ops_iface(op)
            ),
            _ if on_left => format!(
                "define `{name}(other: {other_n})` in `{tn}` or in an `extend {tn}` block"
            ),
            _ => format!(
                "define `static {name}(left: {other_n}, right: {tn})` in `{tn}` or in an `extend {tn}` block"
            ),
        };
        self.cx
            .error(Diagnostic::error(message, span).with_note(format!("help: {help}")));
    }

    /// Both forms accept `l op r`.
    fn ambiguous_operator(&mut self, op: ast::BinaryOp, l: &hir::Expr, r: &hir::Expr, span: Span) {
        let name = operators::binary_method(op).unwrap_or_default();
        let (ln, rn) = (self.cx.display(l.ty), self.cx.display(r.ty));
        let d = Diagnostic::error(
            format!("ambiguous operator `{}` for `{ln}` and `{rn}`", op_str(op)),
            span,
        )
        .with_note(format!("`{ln}`'s method `{name}` takes a `{rn}`"))
        .with_note(format!(
            "`{rn}`'s static `{name}` takes a `{ln}` and a `{rn}`: keep one of them"
        ));
        self.cx.error(d);
    }
}

/// The `velt/ops` interface of binary operator `op`.
fn ops_iface(op: ast::BinaryOp) -> &'static str {
    use ast::BinaryOp as B;
    match op {
        B::Add => "Add",
        B::Sub => "Sub",
        B::Mul => "Mul",
        B::Div => "Div",
        B::Rem => "Rem",
        B::Pow => "Pow",
        B::BitAnd => "BitAnd",
        B::BitOr => "BitOr",
        B::BitXor => "BitXor",
        B::Shl => "Shl",
        B::Shr => "Shr",
        B::UShr => "UShr",
        _ => "Add",
    }
}
