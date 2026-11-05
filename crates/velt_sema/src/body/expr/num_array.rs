//! A packed array literal of number literals (`ast::ExprKind::NumArray`, #2421): checked as the
//! array literal of its elements would be, without making an expression node for each element
//! where that literal's check would not look at one.
//!
//! Against an array of a number type, `array_lit_in` checks each element that is a plain literal
//! as a literal of the element type, in one loop: here the same calls, on each packed value. A
//! negated literal is checked as the expression it stands for (rare in tables). Anywhere else (no
//! element type known yet, `any`, a tuple, a union element, the editor's analysis) the elements
//! become expressions and the array literal takes the ordinary path, so nothing changes there.
//!
//! When every element checks to a number literal of the element type, the result is one
//! `hir::ExprKind::ConstArray` holding their values, which stands for the `ArrayLit` of them;
//! otherwise (a negated literal, an error) it is that `ArrayLit`.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(super) fn num_array(
        &mut self,
        n: &ast::NumArray,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let exp = self.array_expected(exp);
        let Some(elem) = self.num_array_elem(exp) else {
            let elems = n.exprs(span.file);
            return self.array_lit_in(&elems, exp, span);
        };
        // The elements' values while each is a number literal of the element type, then (from
        // the first that is not: a negated literal, an error) the expressions.
        let file = span.file;
        let mut values = Vec::with_capacity(n.elems.len());
        let mut hs: Option<Vec<hir::Expr>> = None;
        let mut id = n.first_id;
        for e in &n.elems {
            let h = match e.negative() {
                false => {
                    crate::check_work::add_expr_check();
                    let h = self.lit(&e.lit(), Some(elem), e.span(file), false);
                    self.coerce(h, elem)
                }
                true => self.expr_coerce(&e.expr(id, file), elem, Want::Move),
            };
            id.0 += e.id_count();
            match &mut hs {
                Some(hs) => hs.push(h),
                None => match hir::ConstElem::of(&h, elem).filter(|_| h.span.file == file) {
                    Some(v) => values.push(v),
                    None => {
                        let mut all = Vec::with_capacity(n.elems.len());
                        all.extend(values.iter().map(|v: &hir::ConstElem| v.expr(elem, file)));
                        all.push(h);
                        hs = Some(all);
                    }
                },
            }
        }
        let ty = self.cx.ty.array(elem);
        match hs {
            Some(hs) => self.mk(H::ArrayLit(hs), ty, span),
            None => {
                let packed = hir::ConstArray { elem, values };
                self.mk(H::ConstArray(Box::new(packed)), ty, span)
            }
        }
    }

    /// The element type of an array literal checked against `exp` (from `array_expected`) when
    /// `array_lit_in` checks its literal elements as literals of it: a number type, known from
    /// the context, and not in the editor's analysis.
    fn num_array_elem(&mut self, exp: Option<TyId>) -> Option<TyId> {
        let t = exp?;
        if matches!(self.cx.ty.kind(t), TyKind::Tuple(_)) {
            return None;
        }
        // No element holds a join (`holds_join`): literals only.
        let elem = self.literal_elem(t, &[])?;
        let number = matches!(self.cx.ty.kind(elem), TyKind::Int(_) | TyKind::Float(_));
        (number && !self.cx.ty.has_error(elem) && !self.cx.recording()).then_some(elem)
    }
}
