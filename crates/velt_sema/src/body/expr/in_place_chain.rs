//! Sorting arrays of `any` values by a comparator that returns a dynamic value or may throw
//! ([`FnCx::dynamic_sort_name`]), and the receiver of a member access that is a call handing
//! back its receiver: `xs.sort().join(",")`, `s.add(1).add(2)` ([`FnCx::in_place_receiver`]).
//! `sort`, `reverse`, `fill` and `copyWithin` return the array as in JavaScript (#3760): they
//! return `this`, so a call hands back its receiver (`crate::hand_back`) and a chain borrows it
//! again.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyKind};

/// The receiver of a member access, with the statements that must run before it.
pub(super) struct Receiver {
    pub before: Vec<hir::Stmt>,
    pub recv: hir::Expr,
}

impl FnCx<'_, '_> {
    /// The method `sort(cmp)` or `toSorted(cmp)` of the array of `any` values of type `recv`
    /// calls (std/prelude/sort.velt): one whose comparator returns a dynamic value, which Node
    /// converts to a number (`true` is 1, `"-1"` is -1, `NaN` is 0; #2577), or else one whose
    /// comparator may throw.
    pub(super) fn dynamic_sort_name(
        &mut self,
        recv: hir::TyId,
        prop: &ast::Ident,
        args: &[ast::Expr],
    ) -> Option<ast::Ident> {
        let ([cmp], "sort" | "toSorted") = (args, prop.name.as_str()) else {
            return None;
        };
        let elem = self.cx.ty.array_elem(recv)?;
        if self.cx.dynamic(elem) != Some(crate::any::Dynamic::Any) {
            return None;
        }
        let name = match (prop.name.as_str(), self.returns_dynamic(cmp)) {
            ("sort", true) => "sortAnyResult",
            ("toSorted", true) => "toSortedAnyResult",
            _ if !self.may_throw(cmp) => return None,
            ("sort", false) => "sortDynamic",
            _ => "toSortedDynamic",
        };
        Some(ast::Ident {
            name: name.to_string(),
            span: prop.span,
        })
    }

    /// Does comparator `cmp` return an `any` or `unknown`: an arrow or a function declared so, or a
    /// variable holding a function that does, or an `any` itself (called through `any`)? An arrow
    /// whose result is an `any` without saying so, or a member (`o.cmp`), is not seen yet: it
    /// sorts as before, and a result that is not a number stops it.
    fn returns_dynamic(&mut self, cmp: &ast::Expr) -> bool {
        let dynamic = |t: &ast::TypeExpr| match &t.kind {
            ast::TypeExprKind::Named { path, args } => {
                args.is_empty()
                    && matches!(path.as_slice(), [p] if p.name == "any" || p.name == "unknown")
            }
            _ => false,
        };
        match &super::method_value::strip_parens(cmp).kind {
            ast::ExprKind::Arrow { ret: Some(r), .. } => dynamic(r),
            ast::ExprKind::Ident(id) => {
                let Some(l) = self.lookup_local(&id.name, id.span) else {
                    // A function declared to return one (`function cmp(a: any, b: any): any`).
                    return match self.lookup_item(&id.name, id.span) {
                        Some(crate::ctx::Item::Def(d))
                            if matches!(
                                self.cx.info[d.0 as usize],
                                crate::defs::DefInfo::Fn(_)
                            ) =>
                        {
                            let ret = self.cx.fn_info(d).ret;
                            self.cx.dynamic(ret).is_some()
                        }
                        _ => false,
                    };
                };
                let t = self.local_ty(l);
                match self.cx.ty.kind(t).clone() {
                    TyKind::FnPtr { ret, .. } => self.cx.dynamic(ret).is_some(),
                    _ => self.cx.dynamic(t).is_some(),
                }
            }
            _ => false,
        }
    }

    /// May comparator `cmp` throw? It writes a `throw`, or in a program with proxies (whose
    /// traps may throw) it reads through its `any` parameters or calls something; a function
    /// named here may do anything there. Any other comparator sorts as before, at its cost.
    fn may_throw(&self, cmp: &ast::Expr) -> bool {
        use velt_syntax::visit::{walk_expr, Visit};
        struct Seen {
            throws: bool,
            reads: bool,
        }
        impl<'a> Visit<'a> for Seen {
            fn stmt(&mut self, s: &'a ast::Stmt) {
                self.throws |= matches!(s.kind, ast::StmtKind::Throw(_));
            }
            fn expr(&mut self, e: &'a ast::Expr) {
                use ast::ExprKind as E;
                self.reads |= matches!(e.kind, E::Member { .. } | E::Index { .. } | E::Call { .. });
            }
        }
        let mut seen = Seen {
            throws: false,
            reads: false,
        };
        let arrow = matches!(
            super::method_value::strip_parens(cmp).kind,
            ast::ExprKind::Arrow { .. }
        );
        walk_expr(cmp, &mut seen);
        seen.throws || (self.uses_proxy() && (seen.reads || !arrow))
    }

    /// `recv`, checked from `object`, as the receiver of a member access: a call that hands back
    /// its receiver (`s.add(1).add(2)`, `xs.sort().join()`, `hand_back`) runs first and the
    /// receiver it hands back is borrowed.
    pub(super) fn in_place_receiver(&mut self, _object: &ast::Expr, recv: hir::Expr) -> Receiver {
        match self.split_hand_back(recv) {
            Ok((before, recv)) => Receiver { before, recv },
            Err(recv) => Receiver {
                before: vec![],
                recv,
            },
        }
    }

    /// `access`, run after `before` (a block expression when there are statements).
    pub(super) fn after_receiver(
        &mut self,
        before: Vec<hir::Stmt>,
        access: hir::Expr,
    ) -> hir::Expr {
        if before.is_empty() {
            return access;
        }
        let (ty, span) = (access.ty, access.span);
        let block = hir::Block {
            stmts: before,
            value: Some(Box::new(access)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}
