//! A large array literal of number literals (`[0x4, 0x13, -2, 1.5, …]`), kept packed: one
//! entry per element instead of an [`Expr`] each (#2421). Generated tables (collation weights,
//! Unicode properties, powers of ten) have thousands of elements, and a full expression node for
//! each costs every pass that walks the tree.
//!
//! The parser makes an [`ExprKind::NumArray`] for an array literal of at least
//! [`NumArray::MIN_LEN`] elements, each a number literal without a type suffix whose integer value
//! fits a `u64`, optionally negated (`-1`, `- 0x2`). [`NumArray::exprs`] gives back the elements
//! exactly as the parser would have made them (the same node ids, spans and literals), so a pass
//! that needs them as expressions treats the node as the [`ExprKind::Array`] it stands for.
//! Every other pass treats it as a leaf: it holds no names, calls or closures.

use super::{Expr, ExprKind, Lit, NodeId, UnaryOp};
use velt_common::{FileId, Span};

#[derive(Clone, Debug, PartialEq)]
pub struct NumArray {
    /// The node id of the first element's first node. The parser numbers the elements' nodes in
    /// order from here: a literal takes one id, a negated literal two (the literal, then the
    /// negation); the array's own id follows the last of them.
    pub first_id: NodeId,
    pub elems: Vec<NumElem>,
}

/// One element: a number literal, maybe negated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumElem {
    /// The literal's value: an integer, or an `f64`'s bits when [`NumElem::float`].
    pub bits: u64,
    /// Start of the element (its `-` when negated).
    pub lo: u32,
    /// Start of the literal (`lo` when not negated).
    pub lit_lo: u32,
    /// End of the element (and of the literal).
    pub hi: u32,
    /// A float literal (`1.5`, `1e3`): `bits` are an `f64`'s.
    pub float: bool,
}

impl NumElem {
    /// Is the literal negated (`-1`)?
    pub fn negative(&self) -> bool {
        self.lit_lo != self.lo
    }

    /// The literal, without its sign.
    pub fn lit(&self) -> Lit {
        match self.float {
            true => Lit::Float {
                value: f64::from_bits(self.bits),
                suffix: None,
            },
            false => Lit::Int {
                value: u128::from(self.bits),
                suffix: None,
            },
        }
    }

    /// The element's span (with its `-`).
    pub fn span(&self, file: FileId) -> Span {
        Span::new(file, self.lo, self.hi)
    }

    /// The literal's span (without its `-`).
    pub fn lit_span(&self, file: FileId) -> Span {
        Span::new(file, self.lit_lo, self.hi)
    }
}

impl NumArray {
    /// The fewest elements the parser packs: below this an ordinary [`ExprKind::Array`] costs
    /// little, and small literals (`[1, 2]`) take every context a literal can.
    pub const MIN_LEN: usize = 16;

    /// The number of node ids the elements take (see [`NumArray::first_id`]).
    pub fn id_count(&self) -> u32 {
        self.elems.iter().map(NumElem::id_count).sum()
    }

    /// The elements as the parser would have made them without packing: the same nodes (ids,
    /// spans, literals) as the [`ExprKind::Array`] this node stands for. `file` is the file of
    /// the array's span.
    pub fn exprs(&self, file: FileId) -> Vec<Expr> {
        let mut id = self.first_id;
        let mut out = Vec::with_capacity(self.elems.len());
        for e in &self.elems {
            out.push(e.expr(id, file));
            id.0 += e.id_count();
        }
        out
    }
}

impl NumElem {
    /// The number of node ids the element takes: 2 when negated (the literal, the negation).
    pub fn id_count(&self) -> u32 {
        1 + u32::from(self.negative())
    }

    /// The element as the parser would have made it, its first node (the literal) numbered
    /// `id`.
    pub fn expr(&self, id: NodeId, file: FileId) -> Expr {
        let lit = Expr {
            id,
            kind: ExprKind::Lit(self.lit()),
            span: self.lit_span(file),
        };
        match self.negative() {
            false => lit,
            true => Expr {
                id: NodeId(id.0 + 1),
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(lit),
                },
                span: self.span(file),
            },
        }
    }
}

impl Expr {
    /// `self` with a packed array of numbers ([`ExprKind::NumArray`]) as the [`ExprKind::Array`]
    /// it stands for; any other expression as it is. For passes that take array literals apart.
    pub fn unpacked(&self) -> std::borrow::Cow<'_, Expr> {
        match &self.kind {
            ExprKind::NumArray(n) => std::borrow::Cow::Owned(Expr {
                id: self.id,
                kind: ExprKind::Array(n.exprs(self.span.file)),
                span: self.span,
            }),
            _ => std::borrow::Cow::Borrowed(self),
        }
    }
}
