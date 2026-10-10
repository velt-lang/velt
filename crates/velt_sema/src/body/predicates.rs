//! Type predicates (TypeScript's user-defined type guards, docs/reference/narrowing.md): a call
//! `isFoo(x)` of `function isFoo(v: A | B): v is A` narrows the local `x` in a condition as a
//! test of its type would: to the members of its union that are `A` (or instances of class `A`)
//! when the call returns `true`, to the others when it returns `false`, and a `T | null` local
//! to `T` when `A` does not admit `null`.
//!
//! TypeScript does not check what a predicate's body returns, so the narrowing cannot be
//! trusted as a `typeof` or `instanceof` test is: a call that narrows checks at run time that
//! the value is what its result says (a tag or class test of the local, like the one `typeof`
//! or `instanceof` compiles to) and throws a `TypeError` when it is not. For a predicate that
//! answers correctly this is the program Node runs.
//!
//! A predicate type that is a literal of a member's type (`x is "a"` on `string | number`)
//! narrows to that member when `true` and excludes nothing when `false`; one with a part no
//! member of the local's type holds (`x is Dog | Cat` on `Animal | string`) does not narrow.

use velt_common::Span;
use velt_syntax::ast;

use super::expr::downcast::{instance_leaf, Instance};
use super::narrow::Fact;
use super::FnCx;
use crate::ctx::Item;
use crate::defs::{DefInfo, FnSource};
use crate::hir::{self, DefId, ExprKind as H, LocalId, PatKind as P, TyId, UseMode};
use crate::resolve::TyEnv;

/// A predicate call whose result narrows a local (`Frame::checked_predicates`).
#[derive(Clone, Debug)]
pub(crate) struct CheckedPredicate {
    pub local: LocalId,
    pub when_true: Vec<Fact>,
    pub when_false: Vec<Fact>,
    /// The predicate's name and type, for the run-time error.
    pub name: String,
    pub ty: String,
}

impl FnCx<'_, '_> {
    /// Facts of a call `f(args)` (at `span`) in a condition, when `f` is a function declared
    /// with a type predicate `p is T` as its result (not generic, so `T` is one type). The call
    /// is recorded so that its lowering checks them ([`Self::predicate_checked`]).
    pub(super) fn predicate_facts(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> (Vec<Fact>, Vec<Fact>) {
        let Some(found) = self.predicate_facts_of(callee, args) else {
            return (vec![], vec![]);
        };
        let out = (found.when_true.clone(), found.when_false.clone());
        self.f.checked_predicates.insert((span.lo, span.hi), found);
        out
    }

    fn predicate_facts_of(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> Option<CheckedPredicate> {
        let (target, index) = self.predicate_of(callee)?;
        let arg = args.get(index)?;
        // The variable itself (not `(x = next())`): the check reads it again after the call.
        if !is_plain_name(arg) {
            return None;
        }
        let (l, nullable, u) = self.local_with_members(arg)?;
        let (when_true, when_false) = match self.cx.class_of(target) {
            Some((class, _)) => self.class_facts(arg, class),
            None => self.predicate_type_facts(l, nullable, u, target)?,
        };
        if when_true.is_empty() && when_false.is_empty() {
            return None;
        }
        let name = match &callee.kind {
            ast::ExprKind::Ident(id) => id.name.clone(),
            _ => String::new(),
        };
        let ty = self.cx.display(target);
        Some(CheckedPredicate {
            local: l,
            when_true,
            when_false,
            name,
            ty,
        })
    }

    /// Facts of a predicate `v is target` (not a class) on local `l` of type `u` (`u | null`
    /// when `nullable`): when `true`, the members that a value of `target` can be (a member
    /// `target` or one of its parts is, or whose type a literal part has); when `false`, the
    /// members `target` does not name exactly. `None` when a part of `target` is no member.
    fn predicate_type_facts(
        &mut self,
        l: LocalId,
        nullable: bool,
        u: TyId,
        target: TyId,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let null_matches = self.cx.ty.opt_payload(target).is_some();
        let wanted: Vec<TyId> = self
            .cx
            .union_members(target)
            .unwrap_or_else(|| vec![target])
            .into_iter()
            .map(|t| self.cx.ty.opt_payload(t).unwrap_or(t))
            .collect();
        let parts = self.cx.union_members(u).unwrap_or_else(|| vec![u]);
        let mut holds: Vec<TyId> = vec![];
        for w in &wanted {
            let base = self.cx.lit_value(*w).map(|v| self.cx.lit_base(&v));
            let found: Vec<TyId> = parts
                .iter()
                .copied()
                .filter(|m| m == w || Some(*m) == base)
                .collect();
            if found.is_empty() {
                return None;
            }
            holds.extend(found);
        }
        let (mut t, mut f) = (vec![], vec![]);
        if self.cx.union_def(u).is_some() {
            let all = 0..parts.len() as u32;
            let yes = all.clone().filter(|&i| holds.contains(&parts[i as usize]));
            let no = all.filter(|&i| !wanted.contains(&parts[i as usize]));
            t.push(Fact::Members(l, yes.collect()));
            f.push(Fact::Members(l, no.collect()));
        }
        if nullable {
            if null_matches {
                f.push(Fact::NonNull(l));
            } else if self.cx.union_def(u).is_some() || holds.contains(&u) {
                t.push(Fact::NonNull(l));
            }
        }
        Some((t, f))
    }

    /// The lowered call `call` (at `span`) of a predicate whose result narrows a local in a
    /// condition, checked against a run-time test of the local so that the narrowing holds
    /// (module docs): `call ? (test_true ? true : throw) : (test_false ? false : throw)`.
    pub(crate) fn predicate_checked(&mut self, call: hir::Expr, span: Span) -> hir::Expr {
        let b = self.cx.ty.bool_;
        if call.ty != b {
            return call;
        }
        let Some(p) = self.f.checked_predicates.get(&(span.lo, span.hi)).cloned() else {
            return call;
        };
        let yes = self.fact_test(p.local, &p.when_true, span);
        let no = self.fact_test(p.local, &p.when_false, span);
        if yes.is_none() && no.is_none() {
            return call;
        }
        let branch = |s: &mut Self, test: Option<hir::Expr>, result: bool| {
            let value = s.mk(H::Lit(hir::Lit::Bool(result)), b, span);
            let Some(test) = test else {
                return value;
            };
            let msg = if result {
                format!(
                    "TypeError: the type predicate `{}` returned true for a value that is not `{}`",
                    p.name, p.ty
                )
            } else {
                format!(
                    "TypeError: the type predicate `{}` returned false for a value that is `{}`",
                    p.name, p.ty
                )
            };
            let msg = s.str_lit(&msg, span);
            let never = s.cx.ty.never;
            let fail = s.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span);
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(value),
                els: Box::new(fail),
            };
            s.mk(kind, b, span)
        };
        let then = branch(self, yes, true);
        let els = branch(self, no, false);
        let kind = H::If {
            cond: Box::new(call),
            then: Box::new(then),
            els: Box::new(els),
        };
        self.mk(kind, b, span)
    }

    /// A test that local `l` (read as its declared type, not as narrowed) satisfies `facts`
    /// about it (`None`: it always does).
    fn fact_test(&mut self, l: LocalId, facts: &[Fact], span: Span) -> Option<hir::Expr> {
        let ty = self.local_ty(l);
        let non_null = facts.contains(&Fact::NonNull(l));
        let mut members: Option<Vec<u32>> = None;
        let mut class: Option<DefId> = None;
        for f in facts {
            match f {
                Fact::Members(x, vs) if *x == l => {
                    members = Some(match members {
                        Some(cur) => cur.into_iter().filter(|v| vs.contains(v)).collect(),
                        None => vs.clone(),
                    });
                }
                Fact::Class(x, t) if *x == l => class = self.cx.class_of(*t).map(|(d, _)| d),
                _ => {}
            }
        }
        let mut alts = vec![];
        let mut restricted = false;
        let mut index = 0u32;
        for (pat, t) in self.member_patterns(ty, span) {
            let Some(t) = t else {
                if non_null {
                    restricted = true;
                } else {
                    alts.push(pat);
                }
                continue;
            };
            let i = index;
            index += 1;
            if members.as_ref().is_some_and(|vs| !vs.contains(&i)) {
                restricted = true;
                continue;
            }
            match class {
                None => alts.push(pat),
                Some(c) => match self.instance_kind(t, c) {
                    Ok(Instance::Yes) => alts.push(pat),
                    Ok(Instance::Maybe(_)) => {
                        restricted = true;
                        alts.push(instance_leaf(pat, c));
                    }
                    _ => restricted = true,
                },
            }
        }
        if !restricted {
            return None;
        }
        let yes = match alts.len() {
            0 => None,
            1 => alts.pop(),
            _ => Some(self.pat(P::Or(alts), ty, span)),
        };
        let read = self.mk(H::Local(l, UseMode::Borrow), ty, span);
        Some(self.bool_match(read, yes, span))
    }

    /// The type predicate's type and parameter index of the function `callee` names.
    fn predicate_of(&mut self, callee: &ast::Expr) -> Option<(TyId, usize)> {
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        let Item::Def(d) = self.cx.lookup_item_at(self.module, &id.name, id.span)? else {
            return None;
        };
        let (decl, module) = match &self.cx.info[d.0 as usize] {
            DefInfo::Fn(f) => match f.source {
                Some(FnSource::Decl(decl)) => (decl, f.module),
                _ => return None,
            },
            _ => return None,
        };
        let ast::TypeExprKind::Predicate {
            param,
            ty: Some(ty),
            asserts: false,
        } = &decl.sig.ret.as_ref()?.kind
        else {
            return None;
        };
        if !decl.sig.generics.is_empty() {
            return None;
        }
        let index = decl
            .sig
            .params
            .iter()
            .position(|p| p.name.name == param.name)?;
        // Resolved again here (its errors were reported with the signature), quietly.
        let mark = crate::body::recheck::Mark::here(self.cx);
        let t = self.cx.resolve_type(ty, &TyEnv::new(module, &[]));
        mark.rollback(self.cx);
        (!self.cx.ty.has_error(t)).then_some((t, index))
    }
}

/// `x` or `this` (possibly parenthesized).
fn is_plain_name(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Paren(inner) => is_plain_name(inner),
        _ => false,
    }
}
