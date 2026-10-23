//! What evaluating an expression can do to other expressions evaluated around it, for the
//! literals and calls that evaluate some of their parts ahead of others (spreads, typed object
//! literals; `written_order.rs`, `spread_array.rs`, `spread_args.rs`): moving a part is only
//! safe when no effect can be seen in between.
//!
//! An expression is told bottom-up from its syntax, checking only its leaves (a name, a member
//! read, which may be a getter) as trials, so a long chain of operators costs one walk, not a
//! check per operator. A checked expression is told by walking it (`hir_effects`).

use velt_syntax::ast;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, BinOp, ExprKind as H, TyId};

/// The effects of evaluating an expression, least first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Effects {
    /// None, and its value is the same wherever it is evaluated: a literal, a closure, a
    /// `const` of a primitive type, or operators over those.
    Constant,
    /// None, but it reads what an effect could change: a `let`, a field, an element.
    Reads,
    /// It may change something or throw: a call (a getter read too), an assignment, `await`,
    /// an integer division.
    Writes,
}

/// What a value is, for the operators over it: they call no user code on primitives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prim {
    /// Not a primitive, or not known to be one.
    No,
    /// An integer (whose division by zero throws).
    Int,
    /// An integer literal, or arithmetic of such: an integer or a number, as the context says.
    MaybeInt,
    /// A number, a boolean or a string.
    Other,
}

impl FnCx<'_, '_> {
    /// The [`Effects`] of evaluating `e` (see the module docs).
    pub(super) fn effects(&mut self, e: &ast::Expr) -> Effects {
        self.ast_effects(e).0
    }

    /// The [`Effects`] of reading the variable `name` (a shorthand property).
    pub(super) fn name_effects(&mut self, name: &ast::Ident) -> Effects {
        let e = super::setters::synth(ast::ExprKind::Ident(name.clone()), name.span);
        self.effects(&e)
    }

    /// [`effects`](Self::effects), and what the value is.
    fn ast_effects(&mut self, e: &ast::Expr) -> (Effects, Prim) {
        use ast::ExprKind as K;
        use Effects::*;
        match &e.kind {
            K::Lit(lit) => match lit {
                ast::Lit::Int { .. } => (Constant, Prim::MaybeInt),
                ast::Lit::Float { .. } | ast::Lit::Str(_) | ast::Lit::Bool(_) => {
                    (Constant, Prim::Other)
                }
                ast::Lit::Null | ast::Lit::Undefined => (Constant, Prim::No),
            },
            K::Arrow { .. } | K::Function(_) => (Constant, Prim::No),
            K::Paren(x) => self.ast_effects(x),
            // A type assertion or a numeric conversion: no effect of its own.
            K::Cast { expr, .. } | K::NonNull(expr) => (self.ast_effects(expr).0, Prim::No),
            K::Ident(_) | K::This | K::Member { .. } | K::Index { .. } => {
                if !super::setters::side_effect_free(e) {
                    return (Writes, Prim::No);
                }
                let h = self.trial_expr(e);
                let prim = self.prim(h.ty);
                (self.hir_effects(&h), prim)
            }
            K::Unary { op, expr } => {
                let (fx, prim) = self.ast_effects(expr);
                match op {
                    ast::UnaryOp::Delete => (Writes, Prim::No),
                    ast::UnaryOp::TypeOf => (fx, Prim::Other),
                    ast::UnaryOp::Void => (fx, Prim::No),
                    _ if prim == Prim::No => (Writes, Prim::No),
                    ast::UnaryOp::Not => (fx, Prim::Other),
                    _ => (fx, prim),
                }
            }
            K::Binary { op, lhs, rhs } => {
                use ast::BinaryOp as B;
                let (l, lp) = self.ast_effects(lhs);
                if l == Writes || *op == B::In {
                    return (Writes, Prim::No);
                }
                let (r, rp) = self.ast_effects(rhs);
                let int = |p| matches!(p, Prim::Int | Prim::MaybeInt);
                // An integer division or remainder by zero throws, and so may an integer power.
                let throws = matches!(op, B::Div | B::Rem | B::Pow) && (int(lp) || int(rp));
                if lp == Prim::No || rp == Prim::No || throws {
                    return (Writes, Prim::No);
                }
                let prim = match op {
                    B::Add | B::Sub | B::Mul | B::Div | B::Rem | B::Pow => match (lp, rp) {
                        (Prim::Int, _) | (_, Prim::Int) => Prim::Int,
                        (Prim::MaybeInt, Prim::MaybeInt) => Prim::MaybeInt,
                        _ => Prim::Other,
                    },
                    B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr | B::UShr => lp,
                    _ => Prim::Other,
                };
                (l.max(r), prim)
            }
            K::Cond { cond, then, els } => {
                let c = self.ast_effects(cond).0;
                let (t, tp) = self.ast_effects(then);
                let (f, fp) = self.ast_effects(els);
                let prim = if tp == Prim::No || fp == Prim::No {
                    Prim::No
                } else if tp == fp {
                    tp
                } else {
                    Prim::Other
                };
                (c.max(t).max(f), prim)
            }
            // A new array of such elements (`[1, x]`): making it has no effect.
            K::Array(xs) => {
                let mut fx = Constant;
                for x in xs {
                    if matches!(x.kind, K::Spread(_)) {
                        return (Writes, Prim::No);
                    }
                    fx = fx.max(self.ast_effects(x).0);
                    if fx == Writes {
                        break;
                    }
                }
                (fx, Prim::No)
            }
            _ => (Writes, Prim::No),
        }
    }

    /// The [`Effects`] of evaluating the checked `h`.
    pub(super) fn hir_effects(&mut self, h: &hir::Expr) -> Effects {
        use Effects::*;
        match &h.kind {
            H::Lit(_) | H::Closure(_) | H::FnRef(..) => Constant,
            H::Local(l, _) => {
                let i = l.0 as usize;
                let fixed = matches!(self.f.kinds.get(i), Some(LocalKind::Const))
                    && self.f.locals.get(i).is_some_and(|d| !d.mutable && !d.boxed);
                match fixed && self.prim(h.ty) != Prim::No {
                    true => Constant,
                    false => Reads,
                }
            }
            H::Global(_) => Reads,
            H::Field { base, .. }
            | H::UnwrapSome(base, _)
            | H::UnwrapVariant { expr: base, .. }
            | H::Downcast(base) => self.hir_effects(base).max(Reads),
            H::Index { base, index, .. } if matches!(index.kind, H::Lit(_)) => {
                self.hir_effects(base).max(Reads)
            }
            H::Cast(x) => self.hir_effects(x),
            H::Unary { expr, .. } if self.prim(expr.ty) != Prim::No => self.hir_effects(expr),
            H::Binary { op, lhs, rhs } => {
                let (lp, rp) = (self.prim(lhs.ty), self.prim(rhs.ty));
                // An integer division or remainder by zero throws, and so may an integer power.
                let int = lp == Prim::Int || rp == Prim::Int;
                let throws = int && matches!(op, BinOp::Div | BinOp::Rem | BinOp::Pow);
                if throws || lp == Prim::No || rp == Prim::No {
                    return Writes;
                }
                self.hir_effects(lhs).max(self.hir_effects(rhs))
            }
            H::Logical { lhs, rhs, .. } => self.hir_effects(lhs).max(self.hir_effects(rhs)),
            H::If { cond, then, els } => {
                let c = self.hir_effects(cond);
                c.max(self.hir_effects(then)).max(self.hir_effects(els))
            }
            // A new array or tuple of such values: making it has no effect.
            H::ArrayLit(xs) | H::Tuple(xs) => {
                let mut fx = Constant;
                for x in xs {
                    fx = fx.max(self.hir_effects(x));
                    if fx == Writes {
                        break;
                    }
                }
                fx
            }
            _ => Writes,
        }
    }

    /// What a value of type `t` is (see [`Prim`]).
    fn prim(&mut self, t: TyId) -> Prim {
        let ty = &self.cx.ty;
        if ty.is_int(t) {
            Prim::Int
        } else if ty.is_numeric(t) || t == ty.bool_ || t == ty.str_ {
            Prim::Other
        } else {
            Prim::No
        }
    }
}
