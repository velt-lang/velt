//! The provider's `jsxSoleEmpty` in precompiled templates (docs/internals/contracts/jsx.md "Sole
//! child"). A runtime that receives one child as itself and several as an array (sigx) may render a
//! `null` or boolean child differently when it is its element's only child. `jsxEscape` and
//! `Fragment` see only the value, so for an element's only `{expr}` or `{...expr}` child that may
//! be `null` or a boolean, the compiler writes the export's string in their place:
//! - a `JSX.Text` child: `{ const t = v; t is text ? jsxEscape(t) : sole }` in the string (just `{
//!   const t = v; sole }` when it is never text, and constant text for a literal);
//! - any other child: the slot `{ const t = v; t is text ? Fragment([t], null) :
//!   jsxTemplate([sole], []) }`.
//!
//! The value is read once, where the child would be.

use velt_common::Span;
use velt_syntax::ast;

use super::children::child_span;
use super::precompile::Template;
use super::provider::{Precompile, Provider};
use super::text_run::Textness;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H};

impl FnCx<'_, '_> {
    /// Write `c`, the only child of an element, into `t`, with `null` and booleans as `sole`.
    pub(super) fn sole_child(
        &mut self,
        p: &Provider,
        pc: Precompile,
        c: &ast::JsxChild,
        sole: &str,
        t: &mut Template,
    ) {
        let span = child_span(c);
        let h = self.child_value(p, c, p.child);
        if self.cx.ty.is_bottom(h.ty)
            || self.compatible(p.element, h.ty)
            || self.textness(&h) == Textness::Always
        {
            return self.dynamic_child(p, pc, h, span, t);
        }
        let never_text = self.textness(&h) == Textness::Never;
        if never_text && matches!(h.kind, H::Lit(_)) {
            // Nothing to evaluate: the export's string is constant text. (A local is still read,
            // so that it is checked like any other use.)
            return t.text.push_str(sole);
        }
        match self.try_coerce(h, pc.text) {
            Ok(h) => {
                let mut lets = vec![];
                let v = self.temp("sole", h, &mut lets);
                let s = if never_text {
                    self.str_lit(sole, span)
                } else {
                    let test = self.is_text_test(&v, span);
                    let escaped = self.escape_call(p, pc, v, span);
                    let empty = self.str_lit(sole, span);
                    self.if_expr(test, escaped, empty, self.cx.ty.str_, span)
                };
                let s = self.with_lets(lets, s);
                self.add_part(t, s);
            }
            Err(h) => {
                let mut lets = vec![];
                let v = self.temp("sole", h, &mut lets);
                let test = self.is_text_test(&v, span);
                let frag = self.fragment_of(p, v, span);
                let empty = self.sole_template(p, pc, sole, span);
                let slot = self.if_expr(test, frag, empty, p.element, span);
                let slot = self.with_lets(lets, slot);
                self.add_slot(p, pc, t, slot);
            }
        }
    }

    /// `jsxTemplateString(sole)`, or `jsxTemplate([sole], [])` without that export.
    fn sole_template(&mut self, p: &Provider, pc: Precompile, sole: &str, span: Span) -> hir::Expr {
        let s = self.str_lit(sole, span);
        if let Some(f) = pc.template_string {
            return self.jsx_call(p, f, "jsxTemplateString", vec![s], span);
        }
        let strings_ty = self.cx.ty.array(self.cx.ty.str_);
        let strings = self.mk(H::ArrayLit(vec![s]), strings_ty, span);
        let slots_ty = self.cx.ty.array(p.element);
        let slots = self.mk(H::ArrayLit(vec![]), slots_ty, span);
        self.jsx_call(p, pc.template, "jsxTemplate", vec![strings, slots], span)
    }

    fn if_expr(
        &mut self,
        cond: hir::Expr,
        then: hir::Expr,
        els: hir::Expr,
        ty: hir::TyId,
        span: Span,
    ) -> hir::Expr {
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        self.mk(kind, ty, span)
    }
}
