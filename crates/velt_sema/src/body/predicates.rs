//! Type predicates (TypeScript's user-defined type guards, docs/reference/narrowing.md): a call
//! `isFoo(x)` of `function isFoo(v: A | B): v is A` narrows the local `x` in a condition as a
//! test of its type would: to the members of its union that are `A` (or instances of class `A`)
//! when the call returns `true`, to the others when it returns `false`, and a `T | null` local
//! to `T` when `A` does not admit `null`.

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;
use crate::ctx::Item;
use crate::defs::{DefInfo, FnSource};
use crate::hir::{DefId, TyId};
use crate::resolve::TyEnv;

impl FnCx<'_, '_> {
    /// Facts of a call `f(args)` in a condition, when `f` is a function declared with a type
    /// predicate `p is T` as its result (not generic, so `T` is one type).
    pub(super) fn predicate_facts(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> (Vec<Fact>, Vec<Fact>) {
        let none = (vec![], vec![]);
        let Some((target, index)) = self.predicate_of(callee) else {
            return none;
        };
        let Some(arg) = args.get(index) else {
            return none;
        };
        if matches!(arg.kind, ast::ExprKind::Spread(_)) {
            return none;
        }
        if let Some((class, _)) = self.cx.class_of(target) {
            return self.predicate_class_facts(arg, class);
        }
        let Some((l, nullable, u)) = self.local_with_members(arg) else {
            return none;
        };
        let wanted = self
            .cx
            .union_members(target)
            .unwrap_or_else(|| vec![target]);
        let null_matches = self.cx.ty.opt_payload(target).is_some();
        let wanted: Vec<TyId> = wanted
            .into_iter()
            .map(|t| self.cx.ty.opt_payload(t).unwrap_or(t))
            .collect();
        let pred = |_: &crate::ctx::Ctx, t: TyId| wanted.contains(&t);
        self.split_facts(l, nullable, u, &pred, null_matches)
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

    /// `isC(x)` for a class `C`: as `x instanceof C`.
    fn predicate_class_facts(&mut self, arg: &ast::Expr, class: DefId) -> (Vec<Fact>, Vec<Fact>) {
        if self.local_with_members(arg).is_none() {
            return (vec![], vec![]);
        }
        self.class_facts(arg, class)
    }
}
