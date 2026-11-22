//! Trace glue through a generator's suspended state (#3119 step 3, docs/internals/design/
//! cycles.md): a generator object held by a weak map value whose state holds the key is an
//! ephemeron cycle like any other, so in a program with weak references the weak core can
//! trace it.
//!
//! The state's layout is known only once spill.rs has placed the poll function's locals, and
//! which of them hold values depends on where the generator is suspended. So the trace runs in
//! the poll function itself, as the close path does: [`TRACE_BIT`] `| k` is a case of the
//! dispatch for each `yield` `k` (and `0`, before the start), which visits what the close path
//! of `k` would drop (the same drop entries and flags, each visited instead of dropped) and
//! returns. `cx` then points to the trace glue's `(visit, ctx)`.
//!
//! - `f$trace(state, visit, ctx)` ([`Work::GenTrace`], `TABLE_TRACE` in the instance's table)
//!   skips a generator that is running, finished or closing (`GEN_RUNNING`, `DONE`, or a tag
//!   with `DROP_BIT` or `CLOSE_BIT`), sets the bit, calls the poll function and puts the tag
//!   back.
//! - The generator class's trace glue calls it through the object's table (glue/trace.rs).
//! - An async generator suspended at an `await` has no case: its state is left out, as are
//!   drop entries the trace does not look into (a value a pattern took parts of, a generator
//!   held inline, an array a `for...of` consumes). Leaving a reference out keeps its target
//!   alive: a leak at worst, never a free of something live.

use velt_sema::hir::{DefId, FnDef, TyId};

use super::{AsyncInfo, TRACE_BIT};
use crate::lower::glue::Glue;
use crate::lower::operand::proj;
use crate::lower::state::DropEntry;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, LState, Work};
use crate::vir::{self, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Byte offset of `f$trace` in a generator's table (after resume, close, free and the slot of
/// close-start or throw), present in a program with weak references.
pub(in crate::lower) const TABLE_TRACE: i128 = 32;

/// The tag bits from `TRACE_BIT` up.
const HIGH_BITS: i128 = 0xE000_0000;

impl Cx<'_> {
    /// Does a generator's table have `f$trace` (a program with weak references)?
    pub(in crate::lower) fn gen_traced(&self) -> bool {
        self.has_weak()
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The visit callback and context of a trace case, read through `cx` (module docs).
    fn trace_cx(&mut self) -> (vir::Local, vir::Local) {
        let pair = self.cx.closure_agg();
        let cx = self.poll_cx();
        let p = self.operand_place(cx, Ty::Ptr);
        let at = proj(&p, Proj::Deref(Ty::Agg(pair)));
        let (visit, ctx) = (self.temp(Ty::Ptr), self.temp(Ty::Ptr));
        self.assign(
            Place::local(visit),
            Rvalue::Use(Operand::Copy(proj(&at, Proj::Field(0)))),
        );
        self.assign(
            Place::local(ctx),
            Rvalue::Use(Operand::Copy(proj(&at, Proj::Field(1)))),
        );
        (visit, ctx)
    }

    /// The trace case of `yield` `k`: what its close path drops, visited (module docs).
    pub(super) fn trace_block(&mut self, k: i128) {
        if !self.cx.gen_traced() {
            return;
        }
        let saved = self.cur;
        let b = self.new_block();
        self.live[b.0 as usize] = true;
        self.switch_to(b);
        let tv = self.trace_cx();
        for i in (0..self.scopes.len()).rev() {
            let drops = self.scopes[i].drops.clone();
            for d in drops.iter().rev() {
                self.trace_entry(d, tv);
            }
        }
        self.terminate(Terminator::Return(cint(0, Ty::U32)));
        self.switch_to(saved);
        self.actx().cases.push((TRACE_BIT | k, b));
    }

    /// The trace case before the start: the owned inputs, as `drop_inputs` drops them.
    pub(super) fn trace_start_case(&mut self, f: &FnDef) {
        if !self.cx.gen_traced() {
            return;
        }
        let saved = self.cur;
        let b = self.new_block();
        self.live[b.0 as usize] = true;
        self.switch_to(b);
        let tv = self.trace_cx();
        for p in &f.params {
            let info = &self.info[p.local.0 as usize];
            // A `weak` variable's capture holds a slot (weak_captures.rs): no edge.
            if info.weak.is_some() {
                continue;
            }
            if info.cell {
                self.trace_cell(p.local, tv);
            } else if info.droppable {
                let ty = info.ty;
                let place = self.local_place(p.local);
                self.trace_part(place, ty, tv);
            }
        }
        self.terminate(Terminator::Return(cint(0, Ty::U32)));
        self.switch_to(saved);
        self.actx().cases.push((TRACE_BIT, b));
    }

    /// One drop entry of the close path, visited (module docs: some are left out).
    fn trace_entry(&mut self, d: &DropEntry, tv: (vir::Local, vir::Local)) {
        match d {
            DropEntry::Temp(p, ty) => self.trace_part(p.clone(), *ty, tv),
            DropEntry::Flagged(p, ty, flag) => {
                let (yes, join) = (self.new_block(), self.new_block());
                self.branch(Operand::Copy(Place::local(*flag)), yes, join);
                self.switch_to(yes);
                self.trace_part(p.clone(), *ty, tv);
                self.goto(join);
                self.switch_to(join);
            }
            DropEntry::Local(id) => {
                let info = &self.info[id.0 as usize];
                // A `weak` variable holds its target's slot, in its cell too: no edge
                // (cycles.md).
                if info.weak.is_some() {
                    return;
                }
                if info.cell {
                    return self.trace_cell(*id, tv);
                }
                let whole = info.moved_fields.is_empty() && info.gen.is_none();
                let (ty, flag, state) = (info.ty, info.flag, info.state);
                if !whole {
                    return;
                }
                let place = self.local_place(*id);
                if let Some(flag) = flag {
                    let (yes, join) = (self.new_block(), self.new_block());
                    self.branch(Operand::Copy(Place::local(flag)), yes, join);
                    self.switch_to(yes);
                    self.trace_part(place, ty, tv);
                    self.goto(join);
                    self.switch_to(join);
                } else if state == LState::Init {
                    self.trace_part(place, ty, tv);
                }
            }
            DropEntry::HalfBuilt(..)
            | DropEntry::Disposing(..)
            | DropEntry::Rest(..)
            | DropEntry::ConsumedArray { .. } => {}
        }
    }

    /// A captured variable's cell held by the state: a counted block, traced like a weak map
    /// value's box (as an environment's cells are, glue/trace_env.rs).
    fn trace_cell(&mut self, id: velt_sema::hir::LocalId, tv: (vir::Local, vir::Local)) {
        let info = &self.info[id.0 as usize];
        let (Some(l), ty) = (info.vir, info.ty) else {
            return;
        };
        if !self.cx.boxing.weak_envs || self.cx.weak_refs_inline(None, ty).is_empty() {
            return;
        }
        let glue = cfunc(self.cx.func(Work::Glue(Glue::WeakBoxTrace, ty)));
        self.call_visit(tv, Operand::Copy(Place::local(l)), glue);
    }

    /// In the trace glue of generator class `gty`, for its object at `o`: the state, through the
    /// instance's `f$trace` in the object's table (none in a program that has no weak references
    /// at the instance's lowering; then nothing is visited).
    pub(in crate::lower) fn trace_gen_state(
        &mut self,
        o: &Place,
        gty: TyId,
        tv: (vir::Local, vir::Local),
    ) {
        let holds = (self.cx.boxing.gen_types.get(&gty).cloned()).unwrap_or_default();
        if !holds
            .iter()
            .any(|&t| !self.cx.weak_refs_inline(None, t).is_empty())
        {
            return;
        }
        let obj = Operand::Copy(o.clone());
        let f = self.gen_table_entry(obj.clone(), TABLE_TRACE);
        let done = self.new_block();
        let nn = self.non_null(f.clone());
        self.when(nn, done);
        let off = self.cx.gen_state_off(gty);
        let state = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, obj, cint(off, Ty::U64)),
        );
        let args = vec![
            state,
            Operand::Copy(Place::local(tv.0)),
            Operand::Copy(Place::local(tv.1)),
        ];
        self.call_entry(f, args, vec![Ty::Ptr; 3], Ty::Unit);
        self.goto(done);
        self.switch_to(done);
    }

    /// [`Work::GenTrace`]: `f$trace(state, visit, ctx)` (module docs).
    pub(in crate::lower) fn build_gen_trace(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let info: AsyncInfo = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("generator state layout unavailable"));
        let name = cx.fn_def(def).name.clone();
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let visit = lw.new_local(Ty::Ptr, Some("visit".into()));
        let ctx = lw.new_local(Ty::Ptr, Some("ctx".into()));
        let tag = Place {
            local: st,
            proj: vec![Proj::Deref(Ty::Agg(info.state)), Proj::Field(0)],
        };
        let t = lw.rvalue_temp(Ty::U32, Rvalue::Use(Operand::Copy(tag.clone())));
        // Running, finished, or closing: nothing to trace. Each such tag has the bit or one
        // above it (`GEN_RUNNING`, `DONE`, `DROP_BIT`, `CLOSE_BIT`); a suspension's has none.
        let high = lw.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::BitAnd, t.clone(), cint(HIGH_BITS, Ty::U32)),
        );
        let suspended = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, high, cint(0, Ty::U32)));
        let done = lw.new_block();
        lw.when(suspended, done);
        let pair = lw.cx.closure_agg();
        let pl = lw.new_local(Ty::Agg(pair), Some("trace".into()));
        lw.assign(
            proj(&Place::local(pl), Proj::Field(0)),
            Rvalue::Use(Operand::Copy(Place::local(visit))),
        );
        lw.assign(
            proj(&Place::local(pl), Proj::Field(1)),
            Rvalue::Use(Operand::Copy(Place::local(ctx))),
        );
        let marked = lw.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::BitOr, t.clone(), cint(TRACE_BIT, Ty::U32)),
        );
        lw.assign(tag.clone(), Rvalue::Use(marked));
        let a = lw.addr(Place::local(pl));
        let r = lw.temp(Ty::U32);
        let args = vec![Operand::Copy(Place::local(st)), a];
        lw.call(
            vir::Callee::Func(info.poll),
            args,
            Some(Place::local(r)),
            false,
        );
        lw.assign(tag, Rvalue::Use(t));
        lw.goto(done);
        lw.switch_to(done);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("{}$trace", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, vec![Ty::Ptr; 3], Ty::Unit)
    }
}
