//! A packed array literal of number constants (`ExprKind::ConstArray`, #2421): what sema makes
//! of a large array literal of number literals checked against an array of a number type, in
//! place of an [`ExprKind::ArrayLit`] of one [`Lit`] expression per element. Additive; part of
//! the hir contract.
//!
//! It stands for exactly that `ArrayLit` ([`ConstArray::exprs`]): each element a `Lit::Int` or
//! `Lit::Float` (never negative: a negated literal keeps the `ArrayLit`) of type
//! [`ConstArray::elem`], with its own span. A pass treats it as that `ArrayLit` would be
//! treated: a new array of constants, with no place, call, closure or effect in it.

use velt_common::{FileId, Span};

use super::{Expr, ExprKind, Lit, TyId};

#[derive(Clone, Debug, PartialEq)]
pub struct ConstArray {
    /// The element type: an integer or float type.
    pub elem: TyId,
    pub values: Vec<ConstElem>,
}

/// One element: the value of its `Lit` and its span.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConstElem {
    /// `Lit::Int`'s value, or `Lit::Float`'s bits when [`ConstElem::float`].
    pub bits: u64,
    pub float: bool,
    pub lo: u32,
    pub hi: u32,
}

impl ConstElem {
    /// The element's literal.
    pub fn lit(&self) -> Lit {
        match self.float {
            true => Lit::Float(f64::from_bits(self.bits)),
            false => Lit::Int(u128::from(self.bits)),
        }
    }

    /// The element, as an expression of the `ArrayLit` the array stands for.
    pub fn expr(&self, elem: TyId, file: FileId) -> Expr {
        Expr {
            kind: ExprKind::Lit(self.lit()),
            ty: elem,
            span: Span::new(file, self.lo, self.hi),
        }
    }

    /// `e` as an element, if it is a number literal of type `elem` (not negated).
    pub fn of(e: &Expr, elem: TyId) -> Option<ConstElem> {
        if e.ty != elem {
            return None;
        }
        let (bits, float) = match &e.kind {
            ExprKind::Lit(Lit::Int(n)) => (u64::try_from(*n).ok()?, false),
            ExprKind::Lit(Lit::Float(f)) => (f.to_bits(), true),
            _ => return None,
        };
        Some(ConstElem {
            bits,
            float,
            lo: e.span.lo,
            hi: e.span.hi,
        })
    }
}

impl ConstArray {
    /// The elements as the expressions of the [`ExprKind::ArrayLit`] this array stands for
    /// (`file`: the file of the array's span).
    pub fn exprs(&self, file: FileId) -> Vec<Expr> {
        self.elem_exprs(file).collect()
    }

    /// [`ConstArray::exprs`] one at a time (no allocation: a number literal owns nothing).
    pub fn elem_exprs(&self, file: FileId) -> impl Iterator<Item = Expr> + '_ {
        self.values.iter().map(move |v| v.expr(self.elem, file))
    }
}

impl Expr {
    /// `self` with a [`ExprKind::ConstArray`] as the [`ExprKind::ArrayLit`] it stands for; any
    /// other expression as it is. For passes that take array literals apart.
    pub fn unpacked(&self) -> std::borrow::Cow<'_, Expr> {
        match &self.kind {
            ExprKind::ConstArray(c) => std::borrow::Cow::Owned(Expr {
                kind: ExprKind::ArrayLit(c.exprs(self.span.file)),
                ty: self.ty,
                span: self.span,
            }),
            _ => std::borrow::Cow::Borrowed(self),
        }
    }
}
