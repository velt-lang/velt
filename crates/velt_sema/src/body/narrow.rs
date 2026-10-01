//! Flow narrowing of locals by the conditions of `if` / `while` / `&&` / `||` / ternaries:
//! - `T | null` locals by null tests (`x != null`, `x == null`) and truthiness (`if (x)`,
//!   `if (!x) return;`): a narrowed local reads as its payload (`ExprKind::UnwrapSome`);
//! - union locals by `typeof x === "tag"`, `x instanceof C` and `x == literal`: a local narrowed
//!   to one member reads as that member (`ExprKind::UnwrapVariant`).
//!
//! `!`, `&&` and `||` combine facts; narrowing lasts until the local is reassigned.

use std::collections::HashSet;

use velt_syntax::ast;

use super::FnCx;
use crate::hir::{LocalId, TyId};

/// What a condition establishes about a local.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Fact {
    /// The `T | null` local is not null.
    NonNull(LocalId),
    /// The union local (or, for `U | null`, its payload when not null) is one of these variants.
    Members(LocalId, Vec<u32>),
}

impl FnCx<'_, '_> {
    /// Facts that hold when `cond` is true / when it is false.
    pub fn narrowing(&mut self, cond: &ast::Expr) -> (Vec<Fact>, Vec<Fact>) {
        use ast::BinaryOp as B;
        match &cond.kind {
            ast::ExprKind::Paren(inner) => self.narrowing(inner),
            ast::ExprKind::Unary {
                op: ast::UnaryOp::Not,
                expr,
            } => {
                let (t, f) = self.narrowing(expr);
                (f, t)
            }
            ast::ExprKind::Binary {
                op: B::And,
                lhs,
                rhs,
            } => {
                let (mut t, _) = self.narrowing(lhs);
                t.extend(self.narrowing(rhs).0);
                (t, vec![])
            }
            ast::ExprKind::Binary {
                op: B::Or,
                lhs,
                rhs,
            } => {
                let (_, mut f) = self.narrowing(lhs);
                f.extend(self.narrowing(rhs).1);
                (vec![], f)
            }
            ast::ExprKind::Binary {
                op: op @ (B::Eq | B::NotEq),
                lhs,
                rhs,
            } => {
                let (t, f) = self.eq_facts(lhs, rhs);
                if *op == B::Eq {
                    (t, f)
                } else {
                    (f, t)
                }
            }
            ast::ExprKind::InstanceOf { expr, ty } => self.instanceof_facts(expr, ty),
            ast::ExprKind::Ident(_)
            | ast::ExprKind::Member {
                optional: false, ..
            }
            | ast::ExprKind::Assign { op: None, .. } => (self.truthy_facts(cond), vec![]),
            _ => (vec![], vec![]),
        }
    }

    /// Facts of a nullable tested for truthiness (`if (user)`): when true, it is not null.
    fn truthy_facts(&mut self, e: &ast::Expr) -> Vec<Fact> {
        if let Some(l) = self.nullable_local(e) {
            return vec![Fact::NonNull(l)];
        }
        if matches!(e.kind, ast::ExprKind::Member { .. }) {
            return self.field_token(e).map(Fact::NonNull).into_iter().collect();
        }
        vec![]
    }

    /// Facts of `lhs == rhs` (true / false).
    fn eq_facts(&mut self, lhs: &ast::Expr, rhs: &ast::Expr) -> (Vec<Fact>, Vec<Fact>) {
        if is_null(lhs) || is_null(rhs) {
            let other = if is_null(rhs) { lhs } else { rhs };
            return match self.nullable_local(other) {
                Some(l) => (vec![], vec![Fact::NonNull(l)]),
                None => match self.field_token(other) {
                    Some(t) => (vec![], vec![Fact::NonNull(t)]),
                    None => (vec![], vec![]),
                },
            };
        }
        if let Some((operand, tag)) = typeof_compare(lhs, rhs) {
            return self.typeof_facts(operand, tag);
        }
        let (other, lit) = match (literal_of(lhs), literal_of(rhs)) {
            (None, Some(l)) => (lhs, l),
            (Some(l), None) => (rhs, l),
            _ => return (vec![], vec![]),
        };
        if let Some(facts) = self.discriminant_facts(other, &lit) {
            return facts;
        }
        let Some((l, nullable, u)) = self.union_local(other) else {
            return (vec![], vec![]);
        };
        let Some((v, _)) = self.lit_member_quiet(u, lit) else {
            return (vec![], vec![]);
        };
        let mut t = vec![Fact::Members(l, vec![v])];
        if nullable {
            t.push(Fact::NonNull(l));
        }
        (t, vec![])
    }

    /// Facts of `x.kind == lit` on a discriminated union local `x`: the members whose
    /// discriminant is `lit` / the others.
    fn discriminant_facts(
        &mut self,
        e: &ast::Expr,
        lit: &ast::SignedLit,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        let l = self.named_local(object)?;
        let ty = self.local_ty(l);
        let nullable = self.cx.ty.opt_payload(ty).is_some();
        let u = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        let values = self.cx.discriminant_values(u, &prop.name)?;
        let (yes, no): (Vec<u32>, Vec<u32>) = (0..values.len() as u32)
            .partition(|v| crate::literals::lit_matches(&values[*v as usize], lit));
        let mut t = vec![Fact::Members(l, yes)];
        if nullable {
            // `x.kind` could only be read once `x` was known to be non-null.
            t.push(Fact::NonNull(l));
        }
        Some((t, vec![Fact::Members(l, no)]))
    }

    /// Facts of `typeof x === tag`.
    fn typeof_facts(&mut self, operand: &ast::Expr, tag: &str) -> (Vec<Fact>, Vec<Fact>) {
        let Some((l, nullable, u)) = self.local_with_members(operand) else {
            return (vec![], vec![]);
        };
        let pred = |cx: &crate::ctx::Ctx, t: TyId| cx.typeof_tag(t) == tag;
        self.split_facts(l, nullable, u, &pred, tag == "object")
    }

    fn instanceof_facts(&mut self, e: &ast::Expr, ty: &ast::TypeExpr) -> (Vec<Fact>, Vec<Fact>) {
        let Some(class) = self.instanceof_class_quiet(ty) else {
            return (vec![], vec![]);
        };
        let Some((l, nullable, u)) = self.local_with_members(e) else {
            return (vec![], vec![]);
        };
        let pred = |cx: &crate::ctx::Ctx, t: TyId| cx.is_instance_of(t, class);
        self.split_facts(l, nullable, u, &pred, false)
    }

    /// Facts for a test that holds for the members satisfying `pred` (and for `null` iff
    /// `null_matches`).
    fn split_facts(
        &mut self,
        l: LocalId,
        nullable: bool,
        u: TyId,
        pred: &dyn Fn(&crate::ctx::Ctx, TyId) -> bool,
        null_matches: bool,
    ) -> (Vec<Fact>, Vec<Fact>) {
        let (mut t, mut f) = (vec![], vec![]);
        if let Some(members) = self.cx.union_members(u) {
            let (yes, no): (Vec<u32>, Vec<u32>) =
                (0..members.len() as u32).partition(|&i| pred(self.cx, members[i as usize]));
            t.push(Fact::Members(l, yes));
            f.push(Fact::Members(l, no));
        }
        if nullable {
            if null_matches {
                f.push(Fact::NonNull(l));
            } else if self.cx.union_def(u).is_some() || pred(self.cx, u) {
                t.push(Fact::NonNull(l));
            }
        }
        (t, f)
    }

    /// A loop may run its body again after an assignment in it: narrowing of every local the
    /// loop assigns does not hold inside (or after) it.
    pub(crate) fn unnarrow_assigned_in(&mut self, lp: &ast::Stmt) {
        let mut names = HashSet::new();
        super::assigned::assigned_in_stmt(lp, &mut names);
        for name in names {
            let found = self
                .f
                .scopes
                .iter()
                .rev()
                .find_map(|s| s.names.get(name).copied());
            if let Some(l) = found {
                self.unnarrow(l);
            }
        }
    }

    /// A named local of the current function: (local, is `T | null`, `T`).
    fn local_with_members(&mut self, e: &ast::Expr) -> Option<(LocalId, bool, TyId)> {
        let l = self.named_local(e)?;
        let ty = self.local_ty(l);
        Some(match self.cx.ty.opt_payload(ty) {
            Some(p) => (l, true, p),
            None => (l, false, ty),
        })
    }

    /// Like [`local_with_members`](Self::local_with_members), for union-typed locals only.
    fn union_local(&mut self, e: &ast::Expr) -> Option<(LocalId, bool, TyId)> {
        let found = self.local_with_members(e);
        found.filter(|(_, _, u)| self.cx.union_def(*u).is_some())
    }

    /// `x` / `this` naming an `Option`-typed local of the current function.
    fn nullable_local(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let l = self.named_local(e)?;
        self.cx.ty.opt_payload(self.local_ty(l)).map(|_| l)
    }

    /// `x` / `this` naming a local of the current function. Inside a closure, a variable of an
    /// enclosing function names the closure's capture of it (created here if needed), so
    /// `(t) => want == null || t === want` narrows `want` in the closure body.
    pub(crate) fn named_local(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let (name, span) = match &e.kind {
            ast::ExprKind::Ident(i) => (i.name.as_str(), i.span),
            ast::ExprKind::This => ("this", e.span),
            ast::ExprKind::Paren(inner) => return self.named_local(inner),
            // `(x = next()) != null` tests the value just stored in `x`.
            ast::ExprKind::Assign {
                op: None, target, ..
            } => return self.named_local(target),
            _ => return None,
        };
        let own = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied());
        match own {
            Some(l) => Some(l),
            None if self.is_local_name(name) => self.lookup_local(name, span),
            None => None,
        }
    }
}

/// `null` (possibly parenthesized).
pub(crate) fn is_null(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => true,
        ast::ExprKind::Paren(inner) => is_null(inner),
        _ => false,
    }
}

/// `typeof x` compared with a string literal (either order): (x, tag).
pub(crate) fn typeof_compare<'e>(
    lhs: &'e ast::Expr,
    rhs: &'e ast::Expr,
) -> Option<(&'e ast::Expr, &'e str)> {
    let operand = |e: &'e ast::Expr| match &e.kind {
        ast::ExprKind::Unary {
            op: ast::UnaryOp::TypeOf,
            expr,
        } => Some(&**expr),
        ast::ExprKind::Paren(inner) => match &inner.kind {
            ast::ExprKind::Unary {
                op: ast::UnaryOp::TypeOf,
                expr,
            } => Some(&**expr),
            _ => None,
        },
        _ => None,
    };
    let tag = |e: &'e ast::Expr| match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    match (operand(lhs), tag(rhs), tag(lhs), operand(rhs)) {
        (Some(x), Some(t), _, _) | (_, _, Some(t), Some(x)) => Some((x, t)),
        _ => None,
    }
}

/// A literal operand (`5`, `-2.5`, `"a"`, `true`) as a pattern literal.
pub(crate) fn literal_of(e: &ast::Expr) -> Option<ast::SignedLit> {
    match &e.kind {
        ast::ExprKind::Lit(l @ (ast::Lit::Int { .. } | ast::Lit::Float { .. })) => {
            Some(ast::SignedLit {
                lit: l.clone(),
                negative: false,
            })
        }
        ast::ExprKind::Lit(l @ (ast::Lit::Str(_) | ast::Lit::Bool(_))) => Some(ast::SignedLit {
            lit: l.clone(),
            negative: false,
        }),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => match &expr.kind {
            ast::ExprKind::Lit(l @ (ast::Lit::Int { .. } | ast::Lit::Float { .. })) => {
                Some(ast::SignedLit {
                    lit: l.clone(),
                    negative: true,
                })
            }
            _ => None,
        },
        ast::ExprKind::Paren(inner) => literal_of(inner),
        _ => None,
    }
}
