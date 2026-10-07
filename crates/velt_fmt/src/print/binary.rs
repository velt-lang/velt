//! Binary operator chains (prettier's `printBinaryishExpressions`): runs of operators of the same
//! precedence are flattened so that, when too long, the chain breaks after every operator of the
//! run at once; operands of other precedences form their own groups.

use velt_syntax::ast::{BinaryOp, Expr, ExprKind};

use super::expr::binary_op;
use super::Printer;
use crate::doc::{cat, concat, group, indent, line, Doc};
use crate::source::strict_equality;

impl<'a> Printer<'a> {
    /// A binary expression; `indent_rest` indents the continuation lines when it breaks.
    pub(super) fn binary(&mut self, e: &Expr, indent_rest: bool) -> Doc {
        let mut parts = self.binary_parts(e);
        let first = parts.remove(0);
        let rest = concat(parts);
        // `a && (` … `)`: the parenthesized element brings its own indentation.
        let jsx_last = matches!(&e.kind, ExprKind::Binary { op, rhs, .. }
            if super::jsx::is_jsx_operand(*op, rhs));
        if indent_rest && !jsx_last {
            group(cat![first, indent(rest)])
        } else {
            group(cat![first, rest])
        }
    }

    /// `[first, " op" line operand, ...]` for the flattened chain rooted at `e`. Iterative:
    /// the parser builds arbitrarily long left-nested chains without recursion.
    fn binary_parts(&mut self, e: &Expr) -> Vec<Doc> {
        // `((a + b) + c) + d` → spine [(+, d), (+, c), (+, b)] with `a` first.
        let mut spine = vec![];
        let mut cur = e;
        while let ExprKind::Binary { op, lhs, rhs } = &cur.kind {
            spine.push((*op, lhs.as_ref(), rhs.as_ref()));
            match &lhs.kind {
                ExprKind::Binary { op: lhs_op, .. } if should_flatten(*op, *lhs_op) => cur = lhs,
                _ => break,
            }
        }
        // The leftmost operand starts where `e` does: its leading comments are already printed.
        let Some(&(_, first, _)) = spine.last() else {
            return vec![self.expr(e)];
        };
        let mut parts = vec![self.expr(first)];
        for &(op, lhs, rhs) in spine.iter().rev() {
            let op_text = self.op_text(op, lhs, rhs);
            if super::jsx::is_jsx_operand(op, rhs) {
                parts.push(cat![" ", op_text, " ", self.expr_jsx_parens(rhs)]);
                continue;
            }
            let right = self.expr(rhs);
            parts.push(if inline_rhs(op, rhs) {
                cat![" ", op_text, " ", right]
            } else {
                cat![" ", op_text, line(), right]
            });
        }
        parts
    }

    fn op_text(&self, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> &'static str {
        let strict = strict_equality(self.src, lhs.span.hi, rhs.span.lo);
        match op {
            BinaryOp::Eq if strict => "===",
            BinaryOp::NotEq if strict => "!==",
            _ => binary_op(op),
        }
    }
}

/// Precedence level (higher binds tighter), as in the parser.
fn precedence(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        Or | Nullish => 1,
        And => 2,
        BitOr => 3,
        BitXor => 4,
        BitAnd => 5,
        Eq | NotEq => 6,
        Lt | LtEq | Gt | GtEq | In => 7,
        Shl | Shr | UShr => 8,
        Add | Sub => 9,
        Mul | Div | Rem => 10,
        Pow => 11,
    }
}

/// Can `parent_op`'s chain absorb a left operand using `child_op`? Same precedence, except where
/// flattening would hide an easily misread grouping (prettier's `shouldFlatten`).
fn should_flatten(parent_op: BinaryOp, child_op: BinaryOp) -> bool {
    use BinaryOp::*;
    if precedence(parent_op) != precedence(child_op) {
        return false;
    }
    !matches!(
        (parent_op, child_op),
        (Pow | Eq | NotEq | Shl | Shr | UShr, _)
            | (Rem, Mul | Div)
            | (Mul | Div, Rem)
            | (Mul, Div)
            | (Div, Mul)
    )
}

/// `a && { ... }` / `a || [ ... ]`: keep a non-empty literal operand on the operator's line.
fn inline_rhs(op: BinaryOp, rhs: &Expr) -> bool {
    matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Nullish)
        && match &rhs.kind {
            ExprKind::Object(props) => !props.is_empty(),
            ExprKind::Array(elems) => !elems.is_empty(),
            _ => false,
        }
}
