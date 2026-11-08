//! Errors: `throw e` and `try { } catch (e) { } finally { }`.
//!
//! A `catch` variable's type is the union of everything its `try` body can throw (direct
//! `throw`s and calls of throwing functions, whose bodies are checked on demand; see
//! `crate::throws`), so it is narrowed with `instanceof` / `switch` like any union; `never`
//! when nothing can throw. Without a `catch`, the body's throws propagate outward.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind, Want};
use crate::defs::ThrowSrc;
use crate::hir::{self, ExprKind as H, LocalId, StmtKind as S, TyId};
use crate::throws::ThrowCheck;

impl FnCx<'_, '_> {
    pub(crate) fn throw_expr(&mut self, e: &ast::Expr, span: Span) -> hir::Expr {
        let h = self.expr(e, None, Want::Move);
        if h.ty == self.cx.ty.unit {
            self.cx.err("cannot throw a `void` value", h.span);
        }
        // Rethrowing a `catch` variable whose `try` cannot throw throws nothing.
        if h.ty != self.cx.ty.never {
            self.throw_src(ThrowSrc::Direct(h.ty, span));
        }
        self.rethrown_srcs(&h);
        self.mk(H::Throw(Box::new(h)), self.cx.ty.never, span)
    }

    pub(crate) fn try_stmt(
        &mut self,
        body: &ast::Block,
        catch: Option<&(Option<ast::Pattern>, ast::Block)>,
        finally: Option<&ast::Block>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.f.tries.push(vec![]);
        let catches = u32::from(catch.is_some());
        self.f.catch_depth += catches;
        let any_calls = self.f.any_calls;
        let (outer, before) = (self.f.locals.len(), self.narrow_state());
        let b = self.block(body);
        // A member read may meet a view of a nothing (`null!` in a `Foo` slot), whose
        // `TypeError` lowering throws to this `catch` as it does for a call through `any`.
        let calls_any = self.f.any_calls != any_calls || super::catch_any::reads_member(body);
        let calls_any = match calls_any {
            false => super::catch_any::AnyTry::No,
            true if before == self.narrow_state()
                && super::catch_any::plain_try(&b, outer, self.cx.ty.never) =>
            {
                super::catch_any::AnyTry::Plain
            }
            true => super::catch_any::AnyTry::Changes,
        };
        self.f.catch_depth -= catches;
        let srcs = self.f.tries.pop().expect("ICE: try stack");
        let calls_any = (calls_any, crate::flow::block_diverges(&b, &self.cx.ty));
        let c = match catch {
            Some((pat, blk)) => Some(self.catch_clause(srcs, pat.as_ref(), blk, calls_any, span)),
            None => {
                srcs.into_iter().for_each(|s| self.throw_src(s));
                None
            }
        };
        let fin = finally.map(|f| self.finally_block(f));
        let kind = S::Try {
            body: b,
            catch: c,
            finally: fin,
        };
        Self::push(out, kind, span);
    }

    /// `catch (e) { ... }`: `e` has the union of what `srcs` throw. A binding-less `catch`
    /// still gets a local when something is caught (lowering stores the error there).
    fn catch_clause(
        &mut self,
        srcs: Vec<ThrowSrc>,
        pat: Option<&ast::Pattern>,
        blk: &ast::Block,
        calls_any: (super::catch_any::AnyTry, bool),
        span: Span,
    ) -> (Option<LocalId>, hir::Block) {
        let ety = self.catch_type(srcs.clone(), span);
        let ety = self.not_fn_catch_type(ety, calls_any, pat, blk, &srcs, span);
        self.handler(srcs, pat, blk, ety, span)
    }

    /// A `catch` clause's variable `pat` of type `ety` declared, and its body `blk` checked.
    pub(super) fn handler(
        &mut self,
        srcs: Vec<ThrowSrc>,
        pat: Option<&ast::Pattern>,
        blk: &ast::Block,
        ety: TyId,
        span: Span,
    ) -> (Option<LocalId>, hir::Block) {
        self.push_scope_until(blk.span.hi);
        let local = match pat.map(|p| (&p.kind, p.span)) {
            Some((ast::PatternKind::Ident(n), _)) => {
                let l = self.declare_local(n, ety, LocalKind::Const);
                if only_rethrown(blk, &n.name) {
                    self.f.caught.push((l, srcs));
                }
                Some(l)
            }
            Some((_, pspan)) => {
                self.cx
                    .err("destructuring a caught value is not supported yet", pspan);
                None
            }
            None if ety != self.cx.ty.never => {
                let n = ast::Ident {
                    name: "<caught>".into(),
                    span,
                };
                Some(self.declare_local(&n, ety, LocalKind::Const))
            }
            None => None,
        };
        let hb = self.block(blk);
        self.pop_scope();
        (local, hb)
    }

    /// `throw e` of a `catch` variable `e` used only that way (so never narrowed: its type is
    /// what the `try` body throws, whatever the sets are in this check): besides the type of
    /// `e` (what the body was known to throw when it was checked), it throws what the body's
    /// sources throw in the final inference. The two agree once inference is done (the `catch`
    /// type is checked against it); while the error types of function types written without
    /// `throws` grow, this carries what a call through one throws on to the function types the
    /// rethrowing closure is stored into within one check, instead of one more check of the
    /// program per closure in a chain of them (`throws::fn_types`).
    fn rethrown_srcs(&mut self, h: &hir::Expr) {
        let H::Local(l, _) = &h.kind else {
            return;
        };
        let found = (self.f.caught.iter()).find(|(c, _)| c == l);
        let Some((_, srcs)) = found.cloned() else {
            return;
        };
        srcs.into_iter().for_each(|s| self.throw_src(s));
    }

    /// The union of what `srcs` throw (`never` when nothing), re-checked after inference.
    fn catch_type(&mut self, srcs: Vec<ThrowSrc>, span: Span) -> TyId {
        let t = crate::throws::srcs_now(self.cx, &srcs);
        // The error-set variables in it have the sets known now (`throws::var_known`).
        let t = self.cx.assume_vars_known(t);
        self.cx.throw_checks.push(ThrowCheck {
            srcs,
            observed: t,
            exact: false,
            span,
        });
        t.unwrap_or(self.cx.ty.never)
    }
}

/// Is every use of the name `name` in `blk` the operand of a `throw` statement (`throw e;`)? A
/// variable used only so is never narrowed or read. Uses of another variable of that name
/// count too (they only make the answer `false`), and nested functions are searched as well.
fn only_rethrown(blk: &ast::Block, name: &str) -> bool {
    struct Uses<'n> {
        name: &'n str,
        uses: usize,
        thrown: usize,
    }
    impl<'a> velt_syntax::visit::Visit<'a> for Uses<'_> {
        fn stmt(&mut self, s: &'a ast::Stmt) {
            if let ast::StmtKind::Throw(e) = &s.kind {
                if matches!(&e.kind, ast::ExprKind::Ident(i) if i.name == self.name) {
                    self.thrown += 1;
                }
            }
        }
        fn expr(&mut self, e: &'a ast::Expr) {
            if matches!(&e.kind, ast::ExprKind::Ident(i) if i.name == self.name) {
                self.uses += 1;
            }
        }
    }
    let wrapped = ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Block(blk.clone()),
            is_async: false,
        },
        span: blk.span,
    };
    let mut u = Uses {
        name,
        uses: 0,
        thrown: 0,
    };
    velt_syntax::visit::walk_expr(&wrapped, &mut u);
    u.uses == u.thrown
}
