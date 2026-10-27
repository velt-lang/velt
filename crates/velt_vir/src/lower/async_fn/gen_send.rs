//! Values and errors passed into a generator (hir_encodings.md "Generators"): `next(value)`
//! and `throw(error)`.
//!
//! - `next(value)` (`GeneratorResumeWith`) resumes with the *resume input* `cx` pointing to the
//!   caller's value (a plain `next()` passes null). The resume block of a `yield` whose type is
//!   `N` (a `Generator<T, E, N>`) copies the value from there; with a null `cx` it is the
//!   `yield`'s second argument (`undefined`), or a panic when `N` has no `undefined`.
//! - `throw(error)` (`GeneratorThrow`, only in a program that may call it:
//!   `Program::generator_throw`) calls the instance's throw function ([`Work::GenThrow`]),
//!   which sets [`THROW_BIT`] in the tag and resumes with `cx` pointing to the error (an
//!   `any`). Dispatch case `THROW_BIT | k` of `yield` `k` inside a `try` with a `catch` throws
//!   a copy of it there; elsewhere it runs the close path (as `return()` does) and reports
//!   [`GEN_REJECTED`]: the caller throws the error itself. So do a generator that has not
//!   started (its arguments are dropped) and a finished one.

use velt_sema::hir::{self, DefId, FnDef, TyId, TyKind};

use super::generator::{GenRef, GEN_RUNNING, GEN_YIELDED};
use super::{CLOSE_BIT, DONE, DROP_BIT};
use crate::lower::operand::proj;
use crate::lower::{cint, unit, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Tag bits of a resume that throws an error in (`throw()`): both other request bits, which no
/// other request of a sync generator sets together, so `THROW_BIT | DONE` differs from `DONE`.
/// (`THROW_BIT | GEN_RUNNING` is `DROP_BIT | GEN_RUNNING`, whose case panics; `THROW_BIT |
/// DONE` is `DROP_BIT | DONE`, whose result a drop ignores.)
pub(super) const THROW_BIT: i128 = DROP_BIT | CLOSE_BIT;
/// Resume result of a `throw()` that nothing in the body caught.
pub(super) const GEN_REJECTED: i128 = 3;
/// Byte offset of a generator table's throw function `(state, error) -> u32`, present when the
/// program may call `throw()`.
pub(super) const TABLE_THROW: i128 = 24;

impl Cx<'_> {
    /// Does the generator being lowered handle errors thrown in (`throw()`)?
    pub(super) fn throws_in(&self) -> bool {
        self.hir.generator_throw
    }
}

impl FnLower<'_, '_> {
    /// Does the generator being lowered handle `throw()` (a sync generator in a program that may
    /// call it)?
    fn handles_throw(&mut self) -> bool {
        self.cx.throws_in() && !self.actx().async_gen
    }

    /// The value of a `yield` of type `ty` once resumed (module docs); `dflt`: its value when
    /// `next()` passed none.
    pub(super) fn received(&mut self, dflt: Option<&hir::Expr>, ty: TyId) -> Operand {
        let nt = self.sub(ty);
        let vt = self.cx.ty(nt);
        if vt == Ty::Unit || self.dead() {
            return unit();
        }
        let dv = dflt.map(|d| {
            let v = self.consume(d);
            self.copy_to_temp(v, vt)
        });
        let out = self.temp(vt);
        let cx = self.poll_cx();
        let none = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, cx.clone(), cint(0, Ty::Ptr)),
        );
        let (none_bb, some_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(none, none_bb, some_bb);
        self.switch_to(none_bb);
        match dv {
            Some(d) => {
                self.assign(
                    Place::local(out),
                    Rvalue::Use(Operand::Copy(Place::local(d))),
                );
                self.goto(join);
            }
            None => self.panic_msg("next() without a value resumed a `yield` that needs one"),
        }
        self.switch_to(some_bb);
        if let Some(d) = dv {
            self.drop_glue(Place::local(d), nt);
        }
        let cp = self.operand_place(cx, Ty::Ptr);
        let v = self.clone_value(Operand::Copy(proj(&cp, Proj::Deref(vt))), nt);
        self.assign(Place::local(out), Rvalue::Use(v));
        self.goto(join);
        self.switch_to(join);
        self.own_value(Operand::Copy(Place::local(out)), nt)
    }

    /// Dispatch case `THROW_BIT | k` of `yield` `k` (module docs).
    pub(super) fn throw_block(&mut self, k: i128) {
        if !self.handles_throw() {
            return;
        }
        let saved = self.cur;
        let d = self.new_block();
        self.live[d.0 as usize] = true;
        self.switch_to(d);
        self.set_tag(GEN_RUNNING);
        self.thrown_here();
        self.switch_to(saved);
        self.actx().cases.push((THROW_BIT | k, d));
    }

    /// The error `cx` points to, thrown at the current point: to the enclosing `catch`, or
    /// (none) closing the generator and reporting [`GEN_REJECTED`].
    fn thrown_here(&mut self) {
        let caught =
            (self.scopes.iter()).any(|s| matches!(s.kind, crate::lower::ScopeKind::Try { .. }));
        let any = (caught && self.cx.hir.any_impl.is_some()).then(|| self.cx.any_ty());
        match any {
            Some(any) => {
                let vt = self.cx.ty(any);
                let cx = self.poll_cx();
                let cp = self.operand_place(cx, Ty::Ptr);
                let e = self.clone_value(Operand::Copy(proj(&cp, Proj::Deref(vt))), any);
                let t = self.copy_to_temp(e, vt);
                self.route_error(Operand::Copy(Place::local(t)), any);
            }
            None => {
                self.emit_drops_from(0);
                if !self.dead() {
                    self.reject();
                }
            }
        }
    }

    /// `YieldDelegate(g)` (hir docs): yield `g`'s value, then resume `g` as this generator was
    /// resumed. True when `g` yielded again.
    pub(in crate::lower) fn yield_delegate(&mut self, g: &hir::Expr) -> Operand {
        if self.dead() {
            return unit();
        }
        let gty = self.sub(g.ty);
        let t = match self.cx.kind(gty) {
            TyKind::Adt(_, a) if !a.is_empty() => a[0],
            _ => crate::lower::ice("`yield*` of something other than a generator"),
        };
        let value = hir::Expr {
            kind: hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::GeneratorValue),
                args: vec![g.clone()],
            },
            ty: t,
            span: g.span,
        };
        let v = self.consume(&value);
        if !matches!(v, Operand::Const(vir::Const::Unit, _)) {
            let p = self.out_place();
            self.store(p, v);
        }
        let more = self.temp(Ty::Bool);
        let (k, resume) = self.suspension();
        self.set_tag(k);
        let yielded = self.gen_code(GEN_YIELDED);
        self.terminate(Terminator::Return(cint(yielded, Ty::U32)));
        self.close_block(k);
        let cont = self.new_block();
        self.delegate_throw_block(k, g, more, cont);
        self.switch_to(resume);
        self.set_tag(GEN_RUNNING);
        let gr = self.gen_ref(g);
        let cx = self.poll_cx();
        let r = self.gen_step(&gr, cx);
        let m = self.gen_result(g, &gr, r, 0);
        self.assign(Place::local(more), Rvalue::Use(m));
        self.goto(cont);
        self.switch_to(cont);
        Operand::Copy(Place::local(more))
    }

    /// Dispatch case `THROW_BIT | k` of `yield*` step `k` over `g`: throw the error into `g`;
    /// if `g` did not catch it, it is thrown at the `yield*` ([`thrown_here`](Self::thrown_here)).
    fn delegate_throw_block(
        &mut self,
        k: i128,
        g: &hir::Expr,
        more: vir::Local,
        cont: vir::BlockId,
    ) {
        if !self.handles_throw() {
            return;
        }
        let saved = self.cur;
        let d = self.new_block();
        self.live[d.0 as usize] = true;
        self.switch_to(d);
        self.set_tag(GEN_RUNNING);
        let gr = self.gen_ref(g);
        let cx = self.poll_cx();
        let r = self.call_gen_throw(&gr, cx);
        self.gen_result(g, &gr, r.clone(), 0);
        let rejected = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, r.clone(), cint(GEN_REJECTED, Ty::U32)),
        );
        let (rej, ok) = (self.new_block(), self.new_block());
        self.branch(rejected, rej, ok);
        self.switch_to(ok);
        let again = Rvalue::Binary(BinOp::Eq, r, cint(GEN_YIELDED, Ty::U32));
        self.assign(Place::local(more), again);
        self.goto(cont);
        self.switch_to(rej);
        self.thrown_here();
        self.switch_to(saved);
        self.actx().cases.push((THROW_BIT | k, d));
    }

    /// Finish a `throw()` nothing caught: done, [`GEN_REJECTED`].
    fn reject(&mut self) {
        self.set_tag(DONE);
        self.terminate(Terminator::Return(cint(GEN_REJECTED, Ty::U32)));
    }

    /// The `throw()` cases of a generator that has not started (drops its arguments, like
    /// closing it) or is finished: [`GEN_REJECTED`].
    pub(super) fn throw_start_cases(&mut self, f: &FnDef) {
        if !self.handles_throw() {
            return;
        }
        let saved = self.cur;
        let d = self.new_block();
        self.live[d.0 as usize] = true;
        self.switch_to(d);
        self.drop_inputs(f);
        self.reject();
        let done = self.new_block();
        self.live[done.0 as usize] = true;
        self.switch_to(done);
        self.terminate(Terminator::Return(cint(GEN_REJECTED, Ty::U32)));
        self.switch_to(saved);
        let cases = &mut self.actx().cases;
        cases.push((THROW_BIT, d));
        cases.push((THROW_BIT | DONE, done));
    }

    /// `GeneratorResumeWith(g, value)`: like `GeneratorResume`, with `cx` pointing to `value`.
    pub(in crate::lower) fn gen_resume_with(&mut self, g: &hir::Expr, v: &hir::Expr) -> Operand {
        let gr = self.gen_ref(g);
        let vt = self.vty(v.ty);
        let val = self.expr(v);
        let at = match vt {
            Ty::Unit => cint(0, Ty::Ptr),
            _ => self.operand_addr(val, vt),
        };
        let r = self.gen_step(&gr, at);
        self.gen_result(g, &gr, r, 0)
    }

    /// `GeneratorThrow(g, error)`: the resume code (module docs); an error the body throws
    /// after catching is rethrown here.
    pub(in crate::lower) fn gen_throw(&mut self, g: &hir::Expr, e: &hir::Expr) -> Operand {
        if !self.cx.throws_in() {
            self.expr(g);
            self.expr(e);
            return cint(GEN_REJECTED, Ty::U32);
        }
        let gr = self.gen_ref(g);
        let et = self.vty(e.ty);
        let ev = self.expr(e);
        let at = self.operand_addr(ev, et);
        let r = self.call_gen_throw(&gr, at);
        self.gen_result(g, &gr, r.clone(), 0);
        r
    }

    /// Call the throw function of generator `gr` with the error at `at`: the resume code.
    fn call_gen_throw(&mut self, gr: &GenRef, at: Operand) -> Operand {
        let state = self.gen_state(gr);
        let r = match gr {
            GenRef::Inline { gen, .. } => {
                let f = self.cx.func(Work::GenThrow(gen.def, gen.targs.clone()));
                let r = self.temp(Ty::U32);
                self.call(
                    vir::Callee::Func(f),
                    vec![state, at],
                    Some(Place::local(r)),
                    false,
                );
                Operand::Copy(Place::local(r))
            }
            GenRef::Boxed { obj, .. } => {
                let f = self.gen_table_entry(obj.clone(), TABLE_THROW);
                self.call_entry(f, vec![state, at], vec![Ty::Ptr, Ty::Ptr], Ty::U32)
            }
        };
        Operand::Copy(Place::local(self.copy_to_temp(r, Ty::U32)))
    }

    /// [`Work::GenThrow`]: `f$throw(state, error)` sets [`THROW_BIT`] and resumes.
    pub(in crate::lower) fn build_gen_throw(
        cx: &mut Cx<'_>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| crate::lower::ice("generator state layout unavailable"));
        let name = cx.fn_def(def).name.clone();
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let err = lw.new_local(Ty::Ptr, Some("error".into()));
        let tag = Place {
            local: st,
            proj: vec![Proj::Deref(Ty::Agg(info.state)), Proj::Field(0)],
        };
        let t = lw.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(
                BinOp::BitOr,
                Operand::Copy(tag.clone()),
                cint(THROW_BIT, Ty::U32),
            ),
        );
        lw.assign(tag, Rvalue::Use(t));
        let r = lw.temp(Ty::U32);
        let args = vec![
            Operand::Copy(Place::local(st)),
            Operand::Copy(Place::local(err)),
        ];
        lw.call(
            vir::Callee::Func(info.poll),
            args,
            Some(Place::local(r)),
            false,
        );
        lw.terminate(Terminator::Return(Operand::Copy(Place::local(r))));
        let sym = format!("{}$throw", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }
}
