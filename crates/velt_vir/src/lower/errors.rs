//! Errors without unwinding. A throwing function returns `Result<ret, E>` through its
//! out-pointer (tag 0 = Ok, 1 = Err). `throw e` and failed calls to throwing functions route the
//! error to the innermost enclosing `try` handler (store into its slot, run the cleanup of the
//! scopes in between, jump), or propagate it by returning `Err`, converting it to the handler's
//! (or the function's) error type on the way (widen.rs). `finally` blocks are scopes that run
//! on every exit edge (drops.rs).

use std::rc::Rc;

use velt_sema::hir::{self, LocalId, TyId, TyKind};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, unit, FnLower, ScopeKind};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Send an owned error value of concrete type `from` to the innermost handler or return it.
    pub(super) fn route_error(&mut self, err: Operand, from: TyId) {
        if self.dead() {
            return;
        }
        let handler = self
            .scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, s)| match s.kind {
                ScopeKind::Try { handler, slot, ty } => Some((i, handler, slot, ty)),
                _ => None,
            });
        if let Some((depth, handler, slot, ty)) = handler {
            let err = self.widen_error(err, from, ty);
            self.store(Place::local(slot), err);
            self.emit_drops_from(depth + 1);
            self.goto(handler);
            return;
        }
        let Some(to) = self.throws else {
            ice("error thrown outside a throwing function or try block")
        };
        let err = self.widen_error(err, from, to);
        self.write_err(err);
        self.emit_drops_from(0);
        self.exit_return(unit());
    }

    /// Write `Err(err)` through the out-pointer (`ret_ty` is the declared or implied Result).
    fn write_err(&mut self, err: Operand) {
        let rty = match self.throws {
            Some(e) => {
                let ret = self.ret_ty.unwrap_or_else(|| ice("ret"));
                self.cx.intern(TyKind::Result(ret, e))
            }
            None => self.ret_ty.unwrap_or_else(|| ice("ret")),
        };
        let rv = self.cx.ty(rty);
        let out = self
            .out_ptr
            .unwrap_or_else(|| ice("Err return without out-pointer"));
        let base = proj(&Place::local(out), Proj::Deref(rv));
        self.assign(proj(&base, Proj::Field(0)), Rvalue::Use(cint(1, Ty::U8)));
        let ev = self.cx.view(rty, 1);
        if self.cx.aggs[ev.0 as usize].fields.len() > 1 {
            self.store(proj(&proj(&base, Proj::Cast(ev)), Proj::Field(1)), err);
        }
    }

    pub(super) fn throw(&mut self, inner: &hir::Expr) -> Operand {
        let from = self.sub(inner.ty);
        let v = self.consume(inner);
        if matches!(self.cx.kind(from), TyKind::Never) {
            // A generic `throw e` instantiated with `E = never`: no such value exists.
            if !self.dead() {
                self.terminate(vir::Terminator::Unreachable);
            }
            return unit();
        }
        self.record_throw_loc();
        self.route_error(v, from);
        unit()
    }

    /// Remember where the error being thrown now came from, for an `Uncaught …` report
    /// (`velt_rt_set_throw_loc`; only when lowering with source locations).
    pub(super) fn record_throw_loc(&mut self) {
        if self.cx.locs.is_none() || self.dead() {
            return;
        }
        let suffix = self.panic_suffix();
        let at = if suffix.is_empty() {
            cint(0, Ty::Ptr)
        } else {
            let id = self.cx.static_str_object(&suffix);
            Operand::Const(vir::Const::Static(id), Ty::Ptr)
        };
        self.call_rt(Rt::SetThrowLoc, vec![at], None);
    }

    pub(super) fn try_stmt(
        &mut self,
        body: &hir::Block,
        catch: Option<&(Option<LocalId>, hir::Block)>,
        finally: Option<&hir::Block>,
    ) {
        if let Some(fin) = finally {
            self.push_scope(ScopeKind::Finally(Some(Rc::new(fin.clone()))));
        }
        let after = self.new_block();
        match catch {
            Some((local, handler)) => self.try_catch(body, *local, handler, after),
            None => {
                self.block(body);
                self.goto(after);
            }
        }
        self.switch_to(after);
        if finally.is_some() {
            self.pop_scope();
        }
    }

    fn try_catch(
        &mut self,
        body: &hir::Block,
        local: Option<LocalId>,
        handler: &hir::Block,
        after: vir::BlockId,
    ) {
        // Sema gives the handler a local whenever something can be caught.
        let err_ty = local.map(|l| self.info[l.0 as usize].ty);
        let err_ty = self.cx.error_ty(err_ty);
        let Some(err_ty) = err_ty else {
            self.block(body);
            self.goto(after);
            return;
        };
        let vt = self.cx.ty(err_ty);
        let slot = self.new_local(vt, Some("caught".into()));
        let handler_bb = self.new_block();
        self.push_scope(ScopeKind::Try {
            handler: handler_bb,
            slot,
            ty: err_ty,
        });
        self.block(body);
        self.pop_scope();
        self.goto(after);
        self.switch_to(handler_bb);
        self.push_scope(ScopeKind::Block);
        match local.and_then(|l| self.local_target(l).map(|p| (l, p))) {
            Some((l, p)) => {
                self.store(p, Operand::Copy(Place::local(slot)));
                if self.info[l.0 as usize].droppable && !self.dead() {
                    self.mark_init(l);
                    self.register_local_drop(l);
                }
            }
            None => self.drop_glue(Place::local(slot), err_ty),
        }
        for s in &handler.stmts {
            self.stmt(s);
        }
        if let Some(v) = &handler.value {
            self.expr_stmt(v);
        }
        self.pop_scope();
        self.goto(after);
    }

    /// `Bool` operand: is the option value at `p` (of concrete type `opt`) non-null?
    pub(super) fn option_is_some(&mut self, p: &Place, opt: TyId) -> Operand {
        match self.cx.ty(opt) {
            Ty::Ptr => self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(vir::BinOp::Ne, Operand::Copy(p.clone()), cint(0, Ty::Ptr)),
            ),
            _ => self.rvalue_temp(
                Ty::Bool,
                Rvalue::Use(Operand::Copy(proj(p, Proj::Field(0)))),
            ),
        }
    }
}
