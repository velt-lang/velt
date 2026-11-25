//! A value that may be `undefined` passed for a parameter with a default (issue #930,
//! https://veltlang.com/docs/reference/types/ "Null and undefined"): JavaScript applies the default where the value
//! is `undefined`. The default of a parameter whose type has no `undefined` is evaluated at the
//! call site (`defaults.rs`), so the argument is tested there:
//! `{ const <arg> = h; typeof <arg> === "undefined" ? <default> : <arg>! }`.

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, ExprKind as H, Nothing, PatKind as P, TyId, TyKind, UseMode};

/// The temporary holding the argument (no source name can be this).
const ARG: &str = "<maybe undefined arg>";

impl FnCx<'_, '_> {
    /// Default `d` (a parameter's, checked once per signature outside any body: `defaults.rs`;
    /// or a field's) placed in this body at `span`, with the type arguments of the call or
    /// literal: its own temporaries (a `Record` literal's, ...) become locals of this body
    /// (`adopt_detached`).
    pub(super) fn placed_default(
        &mut self,
        d: &hir::Expr,
        type_args: &[TyId],
        span: velt_common::Span,
    ) -> hir::Expr {
        let mut d = d.clone();
        crate::visit::map_expr_types(&mut d, &mut |t| self.cx.subst(t, type_args));
        if let Some(throws) = self.cx.default_throws.get(&d.span).cloned() {
            for s in throws {
                let s = s.used_at(span, |t| self.cx.subst(t, type_args));
                self.throw_src(s);
            }
        }
        d.span = span;
        self.adopt_detached(d)
    }

    /// The argument `h` for a parameter of type `target` with the call-site default `default`,
    /// when `h` may be `undefined` and `target` cannot hold it; `Err(h)` otherwise. A
    /// `T | null | undefined` argument for a `T | null` parameter keeps the error: `!` would
    /// drop its `null` too.
    pub(super) fn default_where_undefined(
        &mut self,
        h: hir::Expr,
        target: TyId,
        default: &hir::Expr,
        type_args: &[TyId],
    ) -> Result<hir::Expr, hir::Expr> {
        let undefined = self.cx.ty.undefined();
        let takes_default = self.cx.dynamic(target).is_none()
            && self
                .cx
                .ty
                .nothing_of(target)
                .is_none_or(|n| !n.has_undefined())
            && !self.is_undefined_lit(default);
        if takes_default && h.ty == undefined && matches!(h.kind, H::Lit(hir::Lit::Null)) {
            // `f(undefined)`: the default itself.
            return Ok(self.placed_default(default, type_args, h.span));
        }
        // `T | null | undefined` for a `T | null` parameter: the default where it is
        // `undefined`, the value or its `null` otherwise.
        if takes_default
            && self.cx.dynamic(h.ty).is_none()
            && self.is_three_state(h.ty)
            && self.cx.ty.nothing_of(target) == Some(Nothing::Null)
        {
            return Ok(self.default_where_three_state(h, target, default, type_args));
        }
        let applies = self.cx.dynamic(target).is_none()
            && self.cx.dynamic(h.ty).is_none()
            && matches!(self.cx.ty.kind(h.ty), TyKind::Option(inner, Nothing::Undefined)
                if self.cx.ty.nothing_of(*inner).is_none())
            && self
                .cx
                .ty
                .nothing_of(target)
                .is_none_or(|n| !n.has_undefined())
            && !self.is_undefined_lit(default);
        if !applies {
            return Err(h);
        }
        let span = h.span;
        let mut stmts = vec![];
        self.push_scope();
        self.bind_temp(ARG, h, &mut stmts);
        let name = || {
            super::setters::synth(
                ast::ExprKind::Ident(ast::Ident {
                    name: ARG.into(),
                    span,
                }),
                span,
            )
        };
        let test = crate::body::defaults::undefined_test(name(), span);
        let cond = self.cond(&test);
        let d = self.placed_default(default, type_args, span);
        let value = super::setters::synth(ast::ExprKind::NonNull(Box::new(name())), span);
        let els = self.expr_coerce(&value, target, Want::Move);
        self.pop_scope();
        let pick = H::If {
            cond: Box::new(cond),
            then: Box::new(d),
            els: Box::new(els),
        };
        let pick = self.mk(pick, target, span);
        let b = super::accessor_rmw::block(stmts, Some(pick), span);
        Ok(self.mk(H::Block(b), target, span))
    }

    /// [`Self::default_where_undefined`] of a `T | null | undefined` argument `h` for a
    /// `T | null` parameter: a match on its outer nothing, which is `undefined`
    /// (`three_state`).
    fn default_where_three_state(
        &mut self,
        mut h: hir::Expr,
        target: TyId,
        default: &hir::Expr,
        type_args: &[TyId],
    ) -> hir::Expr {
        let span = h.span;
        let sty = h.ty;
        let mid = (self.cx.ty)
            .opt_payload(sty)
            .expect("ICE: three-state option without a payload");
        let (l, mode) = self.option_binding(&h, mid, ARG, true);
        if mode == UseMode::Move {
            self.force_move(&mut h);
        }
        let value = self.mk(H::Local(l, mode), mid, span);
        let value = self.coerce(value, target);
        let d = self.placed_default(default, type_args, span);
        let bind = self.pat(P::Binding(l, mode), mid, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(bind)), sty, span),
                guard: None,
                body: value,
            },
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: d,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, target, span)
    }

    /// Whether std's bodies are being checked by units, before any program exists (#3789).
    fn std_by_units(&self) -> bool {
        self.cx.units && self.cx.scopes[self.module].is_std
    }

    /// An argument of type `ty` at `span` for a parameter with a default: in std's bodies checked
    /// by units, notes it when it may be `undefined` (the callee's default then applies), for
    /// the call's [`Self::note_left_out`].
    pub(super) fn note_undefined_arg(&mut self, ty: TyId, span: velt_common::Span) {
        let may_be = ty == self.cx.ty.undefined()
            || self.cx.dynamic(ty).is_some()
            || (self.cx.ty.nothing_of(ty)).is_some_and(|n| n.has_undefined());
        if may_be && self.std_by_units() {
            self.undefined_args.push(span);
        }
    }

    /// A call of method `def` in std's bodies, checked by units before any program exists, its
    /// arguments checked since `mark` (`FnCx::undefined_args`). A program's override may give a
    /// parameter whose argument takes the callee's default another default, which JavaScript
    /// applies:
    /// - when only trailing arguments take it (left out, or `undefined` written for them),
    ///   records how many the call writes; once the program's default-filling slots are known,
    ///   `units::std_calls` sends the call to the one taking that many arguments;
    /// - when an argument before them may be `undefined`, or the call takes std's own fill slot
    ///   for some parameters (`general`), records the call as one no slot can redo
    ///   (`Summaries::left_out_general`).
    pub(super) fn note_left_out(
        &mut self,
        (def, general): (DefId, bool),
        args: &[ast::Expr],
        span: velt_common::Span,
        mark: usize,
    ) {
        let undefined: Vec<velt_common::Span> = self.undefined_args.drain(mark..).collect();
        // A method of a class (a subclass may override it), not of a struct or an extension.
        let class = (self.cx.fn_info(def).owner.and_then(|o| self.cx.adt(o)))
            .is_some_and(|a| a.kind == hir::AdtKind::Class && a.decl.is_some());
        if !class || !self.std_by_units() {
            return;
        }
        // A rest parameter takes what follows; a spread's length isn't known here.
        let spread = |a: &ast::Expr| matches!(a.kind, ast::ExprKind::Spread(_));
        if crate::body::defaults::has_rest(self.cx, def) || args.iter().any(spread) {
            return;
        }
        let is_undefined = crate::body::defaults::is_undefined;
        let written = args.len() - args.iter().rev().take_while(|a| is_undefined(a)).count();
        if general || args[..written].iter().any(|a| undefined.contains(&a.span)) {
            self.cx.unit_sums.left_out_general.insert(span);
        } else if written < self.cx.fn_info(def).params.len() {
            self.cx
                .unit_sums
                .left_out_calls
                .insert(span, written as u32);
        }
    }
}
