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
        let b = self.block(body);
        let srcs = self.f.tries.pop().expect("ICE: try stack");
        let c = match catch {
            Some((pat, blk)) => Some(self.catch_clause(srcs, pat.as_ref(), blk, span)),
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
        span: Span,
    ) -> (Option<LocalId>, hir::Block) {
        let ety = self.catch_type(srcs, span);
        self.push_scope_until(blk.span.hi);
        let local = match pat.map(|p| (&p.kind, p.span)) {
            Some((ast::PatternKind::Ident(n), _)) => {
                Some(self.declare_local(n, ety, LocalKind::Const))
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

    /// The union of what `srcs` throw (`never` when nothing), re-checked after inference.
    fn catch_type(&mut self, srcs: Vec<ThrowSrc>, span: Span) -> TyId {
        let t = crate::throws::srcs_now(self.cx, &srcs);
        self.cx.throw_checks.push(ThrowCheck {
            srcs,
            observed: t,
            exact: false,
            span,
        });
        t.unwrap_or(self.cx.ty.never)
    }
}
