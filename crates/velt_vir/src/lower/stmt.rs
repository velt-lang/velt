//! Statement lowering: blocks, `let`, `return`, `if`, `while` (with `step`) and labeled
//! `break`/`continue`, each emitting the cleanup of the scopes it leaves. Destructuring `let`
//! and `for…of` live in matching.rs and for_of.rs, `try` in errors.rs.

use velt_sema::hir::{self, LocalId, TyKind};

use super::operand::proj;
use super::{cint, ice, unit, FnLower, ScopeKind};
use crate::vir::{BlockId, Const, Operand, Place, Proj, Rvalue, Terminator};

impl FnLower<'_, '_> {
    pub(super) fn block(&mut self, b: &hir::Block) {
        self.push_scope(ScopeKind::Block);
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(v) = &b.value {
            self.expr_stmt(v);
        }
        self.pop_scope();
    }

    /// Evaluate for side effects; owned temporaries (including an unused owned result) are dropped.
    pub(super) fn expr_stmt(&mut self, e: &hir::Expr) {
        self.push_scope(ScopeKind::Temps);
        match &e.kind {
            // A spawned task nobody can await: its error is reported as uncaught.
            hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::Spawn),
                args,
            } if args.len() == 1 => {
                self.spawn(&args[0], e.ty, true, false);
            }
            _ => {
                self.expr(e);
            }
        }
        self.pop_scope();
    }

    pub(super) fn stmt(&mut self, s: &hir::Stmt) {
        if self.dead() {
            return;
        }
        let prev = self.enter_span(s.span);
        self.stmt_at(s);
        self.restore_loc(prev);
    }

    /// `stmt` once the location is set.
    fn stmt_at(&mut self, s: &hir::Stmt) {
        use hir::StmtKind as S;
        match &s.kind {
            S::Let { local, init } => self.let_stmt(*local, init.as_ref()),
            S::LetPat { pat, init } => self.let_pat(pat, init),
            S::Expr(e) => self.expr_stmt(e),
            S::Return(e) => self.return_stmt(e.as_ref()),
            S::If { cond, then, els } => self.if_stmt(cond, then, els.as_ref()),
            S::While {
                label,
                cond,
                body,
                step,
            } => self.while_stmt(label, cond, body, step.as_ref()),
            S::ForOf {
                label,
                binding,
                iter,
                body,
                consume,
            } => self.for_of(label, binding, iter, body, *consume),
            S::Try {
                body,
                catch,
                finally,
            } => self.try_stmt(body, catch.as_ref(), finally.as_ref()),
            S::Break(label) => {
                let (depth, brk, _) = self.find_loop(label);
                self.emit_drops_from(depth + 1);
                self.goto(brk);
            }
            S::Continue(label) => {
                let (depth, _, cont) = self.find_loop(label);
                self.emit_drops_from(depth + 1);
                self.goto(cont);
            }
            S::Block(b) => self.block(b),
        }
    }

    fn let_stmt(&mut self, local: LocalId, init: Option<&hir::Expr>) {
        if let Some(
            e @ hir::Expr {
                kind:
                    hir::ExprKind::Call {
                        callee: hir::Callee::Intrinsic(hir::Intrinsic::GeneratorEmbed),
                        ..
                    },
                ..
            },
        ) = init
        {
            if self.let_generator(local, e) {
                return self.own_let(local, init);
            }
        }
        let cell = self.info[local.0 as usize].cell;
        if cell && !self.dead() {
            self.new_cell(local);
        }
        if let Some(e) = init {
            self.push_scope(ScopeKind::Temps);
            let v = self.consume(e);
            if let Some(l) = self.info[local.0 as usize].vir {
                match cell {
                    true => {
                        let p = self.local_place(local);
                        self.store(p, v);
                    }
                    false => self.store(Place::local(l), v),
                }
            }
            self.pop_scope();
        }
        self.own_let(local, init);
    }

    /// After `let local = init`: the scope owns the value (when it needs dropping).
    fn own_let(&mut self, local: LocalId, init: Option<&hir::Expr>) {
        if self.info[local.0 as usize].droppable && !self.dead() {
            match init {
                Some(_) => self.mark_init(local),
                None => self.mark_uninit(local),
            }
            self.register_local_drop(local);
        }
    }

    fn return_stmt(&mut self, e: Option<&hir::Expr>) {
        self.push_scope(ScopeKind::Temps);
        let v = e
            .map(|e| self.consume(e))
            .filter(|v| !matches!(v, Operand::Const(Const::Unit, _)));
        self.emit_return(v);
        self.pop_scope();
    }

    /// Store the result (through the out-pointer for aggregates; as `Ok` for throwing
    /// functions), run every scope's cleanup, return.
    pub(super) fn emit_return(&mut self, v: Option<Operand>) {
        let ret_op = match self.out_ptr {
            Some(_) => {
                if let Some(e) = self.throws {
                    let ret = self.ret_ty.unwrap_or_else(|| ice("ret"));
                    let r = self.cx.intern(TyKind::Result(ret, e));
                    let rv = self.cx.ty(r);
                    let out = self.out_ptr.unwrap_or_else(|| ice("out"));
                    let base = proj(&Place::local(out), Proj::Deref(rv));
                    self.assign(
                        proj(&base, Proj::Field(0)),
                        Rvalue::Use(cint(0, crate::vir::Ty::U8)),
                    );
                }
                if let Some(v) = v {
                    let p = self.out_place();
                    self.store(p, v);
                }
                unit()
            }
            None => match v {
                Some(Operand::Copy(p)) if !p.proj.is_empty() => self.read_now(p),
                v => v.unwrap_or_else(unit),
            },
        };
        self.emit_drops_from(0);
        self.exit_return(ret_op);
    }

    /// Copy the returned place into a temporary before the cleanup runs: a projection can point
    /// into a value that cleanup frees (`return dp[0]` reads `dp`'s buffer), so reading it in the
    /// `return` terminator would read freed memory.
    fn read_now(&mut self, p: Place) -> Operand {
        let ret = self
            .ret_ty
            .unwrap_or_else(|| ice("a returned place without a return type"));
        let ty = self.cx.ty(ret);
        let t = self.temp(ty);
        self.assign(Place::local(t), Rvalue::Use(Operand::Copy(p)));
        Operand::Copy(Place::local(t))
    }

    /// The function's final `return` terminator (after the result is stored and scopes are
    /// cleaned up). A poll function marks its state finished and reports READY instead.
    pub(super) fn exit_return(&mut self, op: Operand) {
        if self.asyncx.is_some() {
            self.finish_poll();
        } else {
            self.terminate(Terminator::Return(op));
        }
    }

    fn if_stmt(&mut self, cond: &hir::Expr, then: &hir::Block, els: Option<&hir::Block>) {
        self.push_scope(ScopeKind::Temps);
        let c = self.expr(cond);
        self.pop_scope();
        let then_bb = self.new_block();
        let join = self.new_block();
        let else_bb = if els.is_some() {
            self.new_block()
        } else {
            join
        };
        self.branch(c, then_bb, else_bb);
        self.switch_to(then_bb);
        self.block(then);
        self.goto(join);
        if let Some(els) = els {
            self.switch_to(else_bb);
            self.block(els);
            self.goto(join);
        }
        self.switch_to(join);
    }

    fn while_stmt(
        &mut self,
        label: &Option<String>,
        cond: &hir::Expr,
        body: &hir::Block,
        step: Option<&hir::Expr>,
    ) {
        let cond_bb = self.new_block();
        let body_bb = self.new_block();
        let step_bb = if step.is_some() {
            self.new_block()
        } else {
            cond_bb
        };
        let exit = self.new_block();
        self.goto(cond_bb);
        self.switch_to(cond_bb);
        self.push_scope(ScopeKind::Temps);
        let c = self.expr(cond);
        self.pop_scope();
        self.branch(c, body_bb, exit);
        self.switch_to(body_bb);
        // The loop scope also covers `step`: sema's do-while encoding puts `if (!c) break;` into
        // the step, and that break targets this loop.
        self.push_scope(ScopeKind::Loop {
            label: label.clone(),
            brk: exit,
            cont: step_bb,
        });
        self.block(body);
        self.goto(step_bb);
        if let Some(st) = step {
            self.switch_to(step_bb);
            self.expr_stmt(st);
            self.goto(cond_bb);
        }
        self.pop_scope();
        self.switch_to(exit);
    }

    /// (scope index, break target, continue target) of the innermost matching loop.
    pub(super) fn find_loop(&self, label: &Option<String>) -> (usize, BlockId, BlockId) {
        for (i, s) in self.scopes.iter().enumerate().rev() {
            if let ScopeKind::Loop {
                label: l,
                brk,
                cont,
            } = &s.kind
            {
                if label.is_none() || l == label {
                    return (i, *brk, *cont);
                }
            }
        }
        ice(format_args!("break/continue target {label:?} not found"))
    }
}
