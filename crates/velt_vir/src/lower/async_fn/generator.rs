//! Generators (`function*`; hir_encodings.md "Generators", docs/internals/design/iteration.md):
//! the async state machine with `yield` as a suspension.
//!
//! - The poll function of a generator instance is its *resume* function: `f$poll(state, cx)`
//!   with a null `cx` (it never awaits) returning [`GEN_DONE`], [`GEN_YIELDED`] (the value is in
//!   the result region at offset 0: the `Ok` payload of `Result<T, E>` when the body throws,
//!   else the `T` itself) or [`GEN_THREW`] (the region holds `Err(e)`). Each `yield` stores the
//!   value, sets the tag and returns; its resume block continues after the `yield`.
//! - The *close* path (`f$drop`, `return()`, dropping the generator): the dispatch case
//!   `DROP_BIT | k` of `yield` `k` runs everything a `return;` there would run — the `finally`
//!   blocks and drops (`using` disposals) of the scopes the `yield` is in — then finishes. A
//!   generator that never started drops its arguments; a finished one does nothing.
//! - A generator *value* (`Generator<T, E>`, a prelude class) is one heap object
//!   `[table: ptr][state…]` ([`GEN_STATE_OFF`]); the table holds the instance's resume, close
//!   and free functions (`gen_object.rs`). A `for...of` over a direct call keeps the state in a
//!   frame local instead (`GeneratorEmbed`): resumed directly, closed by its drop.

use velt_sema::hir::{self, DefId, FnDef, PassMode, TyId, TyKind};

use super::{AsyncInfo, DROP_BIT};
use crate::lower::operand::proj;
use crate::lower::{cint, unit, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Resume results.
pub(super) const GEN_DONE: i128 = 0;
pub(super) const GEN_YIELDED: i128 = 1;
pub(super) const GEN_THREW: i128 = 2;
/// Tag of a generator whose body is running: resuming it again from inside (through a
/// reference it holds to itself) panics, as JS throws "Generator is already running".
pub(super) const GEN_RUNNING: i128 = 0x7FFF_FFFE;
/// Byte offset of the state in a generator object, after the table pointer (states are at most
/// 8-aligned: no VIR scalar is wider).
pub(in crate::lower) const GEN_STATE_OFF: i128 = 8;
/// Byte offsets of the table entries: resume `(state, cx) -> u32`, close `(state)`, free `(obj)`.
pub(super) const TABLE_RESUME: i128 = 0;
pub(super) const TABLE_CLOSE: i128 = 8;
pub(super) const TABLE_FREE: i128 = 16;

/// A local holding a generator's state inline (`GeneratorEmbed`).
#[derive(Clone, Debug)]
pub(in crate::lower) struct GenLocal {
    pub def: DefId,
    pub targs: Vec<TyId>,
    pub info: AsyncInfo,
}

/// Where a generator operand's state is and how to drive it.
enum GenRef {
    /// Inline state at `state` (a pointer): direct calls of the instance's functions.
    Inline { state: Operand, gen: GenLocal },
    /// A generator object: through its table.
    Boxed { obj: Operand },
}

impl Cx<'_> {
    /// `[T, E]` of a generator's declared result (`Generator<T, E>`, `Iterator<T, E>` or
    /// `Iterable<T, E>`), substituted.
    pub(in crate::lower) fn gen_args(&mut self, f: &FnDef, targs: &[TyId]) -> (TyId, TyId) {
        let ret = self.subst(f.ret, targs);
        match self.kind(ret) {
            TyKind::Adt(_, a) | TyKind::Dyn(_, a) if a.len() == 2 => (a[0], a[1]),
            _ => crate::lower::ice("generator result is not a generator type"),
        }
    }

    /// `[T, E]` of a `Generator<T, E>` value type.
    fn gen_value_args(&mut self, t: TyId) -> (TyId, TyId) {
        match self.kind(t) {
            TyKind::Adt(_, a) if a.len() == 2 => (a[0], a[1]),
            _ => crate::lower::ice("generator operand is not a `Generator<T, E>`"),
        }
    }
}

impl FnLower<'_, '_> {
    /// Is the function being lowered a generator's resume function?
    pub(in crate::lower) fn in_generator(&self) -> bool {
        self.asyncx.as_ref().is_some_and(|a| a.generator)
    }

    /// `yield v` (`Intrinsic::Yield`) in a generator body (module docs).
    pub(in crate::lower) fn yield_value(&mut self, v: &hir::Expr) -> Operand {
        let value = self.consume(v);
        if self.dead() {
            return unit();
        }
        if !matches!(value, Operand::Const(vir::Const::Unit, _)) {
            let p = self.out_place();
            self.store(p, value);
        }
        let (k, resume) = self.suspension();
        self.set_tag(k);
        self.terminate(Terminator::Return(cint(GEN_YIELDED, Ty::U32)));
        self.close_block(k);
        self.switch_to(resume);
        self.set_tag(GEN_RUNNING);
        unit()
    }

    /// The dispatch case of a running generator (module docs of [`GEN_RUNNING`]).
    pub(super) fn running_case(&mut self) {
        let saved = self.cur;
        let b = self.new_block();
        self.live[b.0 as usize] = true;
        self.switch_to(b);
        let msg = self.str_lit("generator is already running");
        let p = self.operand_addr(msg, Ty::Agg(vir::STR_AGG));
        self.call_rt(crate::lower::rt::Rt::Panic, vec![p], None);
        self.switch_to(saved);
        self.actx().cases.push((GEN_RUNNING, b));
    }

    /// The close path of `yield` `k`: what `return;` at the `yield` runs (module docs).
    fn close_block(&mut self, k: i128) {
        let saved = self.cur;
        let d = self.new_block();
        self.live[d.0 as usize] = true;
        self.switch_to(d);
        self.set_tag(GEN_RUNNING);
        self.emit_drops_from(0);
        if !self.dead() {
            self.finish_poll();
        }
        self.switch_to(saved);
        self.actx().cases.push((DROP_BIT | k, d));
    }

    /// The generator operand `g` (a `Generator<T, E>` place).
    fn gen_ref(&mut self, g: &hir::Expr) -> GenRef {
        if let hir::ExprKind::Local(l, _) = &g.kind {
            if let Some(gen) = self.info[l.0 as usize].gen.clone() {
                let p = self.local_place(*l);
                let state = self.addr(p);
                return GenRef::Inline { state, gen };
            }
        }
        let v = self.expr(g);
        let obj = self.copy_to_temp(v, Ty::Ptr);
        GenRef::Boxed {
            obj: Operand::Copy(Place::local(obj)),
        }
    }

    /// Pointer to the state of `g`.
    fn gen_state(&mut self, g: &GenRef) -> Operand {
        match g {
            GenRef::Inline { state, .. } => state.clone(),
            GenRef::Boxed { obj } => self.rvalue_temp(
                Ty::Ptr,
                Rvalue::Binary(BinOp::PtrAdd, obj.clone(), cint(GEN_STATE_OFF, Ty::U64)),
            ),
        }
    }

    /// Entry `off` of the table of generator object `obj`.
    pub(in crate::lower) fn gen_table_entry(&mut self, obj: Operand, off: i128) -> Operand {
        let op = self.operand_place(obj, Ty::Ptr);
        let table = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&op, Proj::Deref(Ty::Ptr)))),
        );
        let at = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, table, cint(off, Ty::U64)),
        );
        let ap = self.operand_place(at, Ty::Ptr);
        self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&ap, Proj::Deref(Ty::Ptr)))),
        )
    }

    /// Run `g` to its next `yield` or its end: the resume result (`u32`).
    fn gen_step(&mut self, g: &GenRef) -> Operand {
        let state = self.gen_state(g);
        let null = cint(0, Ty::Ptr);
        match g {
            GenRef::Inline { gen, .. } => {
                let r = self.temp(Ty::U32);
                let callee = vir::Callee::Func(gen.info.poll);
                self.call(callee, vec![state, null], Some(Place::local(r)), false);
                Operand::Copy(Place::local(r))
            }
            GenRef::Boxed { obj } => {
                let f = self.gen_table_entry(obj.clone(), TABLE_RESUME);
                self.call_entry(f, vec![state, null], vec![Ty::Ptr, Ty::Ptr], Ty::U32)
            }
        }
    }

    /// `GeneratorResume(g)`: true when a value was yielded; an error thrown by the body is
    /// rethrown here (to the enclosing handler, or out of this function).
    pub(in crate::lower) fn gen_resume(&mut self, g: &hir::Expr) -> Operand {
        let gty = self.sub(g.ty);
        let (t, e) = self.cx.gen_value_args(gty);
        let gr = self.gen_ref(g);
        let r = self.gen_step(&gr);
        let yielded = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, r.clone(), cint(GEN_YIELDED, Ty::U32)),
        );
        if let Some(e) = self.cx.error_ty(Some(e)) {
            let threw = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, r, cint(GEN_THREW, Ty::U32)),
            );
            let (err_bb, ok_bb) = (self.new_block(), self.new_block());
            self.branch(threw, err_bb, ok_bb);
            self.switch_to(err_bb);
            let state = self.gen_state(&gr);
            let rty = self.cx.intern(TyKind::Result(t, e));
            let rv = self.cx.ty(rty);
            let sp = self.operand_place(state, Ty::Ptr);
            let ev = self.cx.view(rty, 1);
            let payload = proj(
                &proj(&proj(&sp, Proj::Deref(rv)), Proj::Cast(ev)),
                Proj::Field(1),
            );
            let et = self.cx.ty(e);
            let err = self.copy_to_temp(Operand::Copy(payload), et);
            self.route_error(Operand::Copy(Place::local(err)), e);
            self.switch_to(ok_bb);
        }
        yielded
    }

    /// `GeneratorValue(g)`: the value of the last `yield`, moved out of the result region.
    pub(in crate::lower) fn gen_value(&mut self, g: &hir::Expr) -> Operand {
        let gty = self.sub(g.ty);
        let (t, e) = self.cx.gen_value_args(gty);
        let gr = self.gen_ref(g);
        let state = self.gen_state(&gr);
        let sp = self.operand_place(state, Ty::Ptr);
        let vt = self.cx.ty(t);
        if vt == Ty::Unit {
            return unit();
        }
        let slot = match self.cx.error_ty(Some(e)) {
            Some(e) => {
                let rty = self.cx.intern(TyKind::Result(t, e));
                let rv = self.cx.ty(rty);
                let ov = self.cx.view(rty, 0);
                proj(
                    &proj(&proj(&sp, Proj::Deref(rv)), Proj::Cast(ov)),
                    Proj::Field(1),
                )
            }
            None => proj(&sp, Proj::Deref(vt)),
        };
        self.own_value(Operand::Copy(slot), t)
    }

    /// `GeneratorReturn(g)`: close `g` (module docs).
    pub(in crate::lower) fn gen_return(&mut self, g: &hir::Expr) -> Operand {
        let gr = self.gen_ref(g);
        let state = self.gen_state(&gr);
        match &gr {
            GenRef::Inline { gen, .. } => {
                let f = self.cx.func(Work::AsyncDrop(gen.def, gen.targs.clone()));
                self.call(vir::Callee::Func(f), vec![state], None, false);
            }
            GenRef::Boxed { obj } => {
                let f = self.gen_table_entry(obj.clone(), TABLE_CLOSE);
                self.call_entry(f, vec![state], vec![Ty::Ptr], Ty::Unit);
            }
        }
        unit()
    }

    /// `let <generator> = GeneratorEmbed(gen(args))`: build the state in the local itself.
    /// Returns false when the instance's layout is not available (a generator iterating a
    /// direct call of itself): the local then holds a generator object as usual.
    pub(in crate::lower) fn let_generator(
        &mut self,
        local: hir::LocalId,
        init: &hir::Expr,
    ) -> bool {
        let Some((def, targs, args)) = embedded_call(init) else {
            return false;
        };
        let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
        let Some(info) = self.cx.async_info(def, &targs) else {
            return false;
        };
        if self.dead() {
            return true;
        }
        let f = self.cx.fn_def(def);
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        self.push_scope(crate::lower::ScopeKind::Temps);
        let vals = self.async_args(args, &modes);
        let st = self.new_local(Ty::Agg(info.state), Some(f.name.clone()));
        self.init_state(&info, &Place::local(st), vals);
        self.pop_scope();
        let i = &mut self.info[local.0 as usize];
        i.vir = Some(st);
        i.indirect = false;
        i.gen = Some(GenLocal { def, targs, info });
        true
    }

    /// Drop of an inline generator state at `place`: close it.
    pub(in crate::lower) fn drop_gen_local(&mut self, place: Place, gen: &GenLocal) {
        let f = self.cx.func(Work::AsyncDrop(gen.def, gen.targs.clone()));
        let a = self.addr(place);
        self.call(vir::Callee::Func(f), vec![a], None, false);
    }

    /// `GeneratorEmbed(call)` outside a `let` it can embed into: the call's generator object.
    pub(in crate::lower) fn gen_embed_value(&mut self, init: &hir::Expr) -> Operand {
        let Some((def, targs, args)) = embedded_call(init) else {
            crate::lower::ice("GeneratorEmbed of something other than a generator call")
        };
        let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
        self.gen_new_call(def, targs, args)
    }
}

/// The generator call inside `GeneratorEmbed(call)`.
fn embedded_call(init: &hir::Expr) -> Option<(DefId, &[TyId], &[hir::Expr])> {
    let call = match &init.kind {
        hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::GeneratorEmbed),
            args,
        } => args.first()?,
        _ => init,
    };
    match &call.kind {
        hir::ExprKind::Call {
            callee: hir::Callee::Def(d, targs),
            args,
        } => Some((*d, targs.as_slice(), args.as_slice())),
        _ => None,
    }
}
