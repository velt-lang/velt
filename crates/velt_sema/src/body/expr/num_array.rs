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

use crate::body::FnCx;
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
        let fast = self.num_elem_kind(elem);
        let mut values = Vec::with_capacity(n.elems.len());
        let mut hs: Option<Vec<hir::Expr>> = None;
        let mut id = n.first_id;
        for e in &n.elems {
            // A plain literal that fits: its value as `lit` and `coerce` would make it (#3457).
            if hs.is_none() {
                if let Some(v) = fast.and_then(|k| self.num_elem_fast(e, k, file)) {
                    values.push(v);
                    id.0 += e.id_count();
                    continue;
                }
            }
            // A literal an integer array's element converts (`256`, `-1`, `2.5` in a `u8[]`,
            // #3182) as the unpacked literal is (`element_lit_coerce`).
            let h = match e.negative() || self.number_stores_into(elem) {
                false => {
                    crate::check_work::add_expr_check();
                    let h = self.lit(&e.lit(), Some(elem), e.span(file), false);
                    self.coerce(h, elem)
                }
                true => self.element_lit_coerce(&e.expr(id, file), elem),
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

    /// How [`Self::num_elem_fast`] checks a literal of element type `elem`: against an integer
    /// type's range, or as a value of `f32`/`f64` (`true` for `f64`, whose integer literals
    /// `literal_locals` notes). `None` for any other type (`f16`, `bf16`: their literals round
    /// and have a range of their own), which takes `lit`.
    fn num_elem_kind(&self, elem: TyId) -> Option<FastElem> {
        let ty = &self.cx.ty;
        if ty.is_storage_float(elem) {
            return None;
        }
        match ty.kind(elem) {
            TyKind::Int(it) => Some(FastElem::Int(*it)),
            TyKind::Float(_) if ty.is_float(elem) => Some(FastElem::Float(elem == ty.f64)),
            _ => None,
        }
    }

    /// The element `e` (not negated) as `lit` then `coerce` would check it against the element
    /// type, without making its expression: a literal that fits an integer type, or any literal
    /// of a float type. `None` for every other case (a negated or out-of-range literal, a float
    /// literal for an integer type), which `lit` checks and reports as it does elsewhere.
    fn num_elem_fast(
        &mut self,
        e: &ast::NumElem,
        k: FastElem,
        file: velt_common::FileId,
    ) -> Option<hir::ConstElem> {
        if e.negative() {
            return None;
        }
        let span = e.span(file);
        let (bits, float) = match k {
            FastElem::Int(it) => {
                let fits = !e.float && u128::from(e.bits) <= crate::types::int_max(it, false);
                fits.then_some((e.bits, false))?
            }
            FastElem::Float(f64_elem) => match e.float {
                true => (e.bits, true),
                false => {
                    if f64_elem {
                        self.literal_number_lit(span);
                    }
                    ((e.bits as f64).to_bits(), true)
                }
            },
        };
        crate::check_work::add_expr_check();
        Some(hir::ConstElem {
            bits,
            float,
            lo: span.lo,
            hi: span.hi,
        })
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

/// How a packed element is checked without its expression ([`FnCx::num_elem_kind`]).
#[derive(Clone, Copy)]
enum FastElem {
    Int(hir::IntTy),
    /// A float type; `true` for `f64`.
    Float(bool),
}
