//! Trace glue through function values (docs/internals/design/cycles.md "Compilation"): a
//! closure's heap environment is a counted object that owns its captures, so a weak map value
//! holding a handler closure that captures the key is an ephemeron cycle like any other (#989).
//!
//! A function value's static type does not say which closure's environment it holds, and the
//! environment's header has no room for a trace entry (environments the runtime makes have the
//! same three words: velt_rt `fn_props.rs`). So the trace glue of a function value of type `F`
//! compares the environment's drop entry, which is per closure instance (`Work::EnvDrop`), with
//! those of the closures made at `F`'s key (`Boxing::env_traced`, cycles_keys.rs), and visits
//! the environment with the matching closure's trace glue (`Work::EnvTrace`). An environment
//! that matches none (one the runtime made, or one whose drop entry a function property
//! replaced) is left out: a leak at worst, never a free of something live, as the weak core
//! requires of every trace glue (rt_abi.md "Weak references").
//!
//! Two closures' drop entries can share an address when the linker folds identical functions
//! (`/OPT:ICF` in Windows release links, velt_link). The dispatch stays sound: identical drop
//! code releases the same fields with the same glue, so the environments have the same owned
//! captures at the same offsets, and either closure's trace glue visits exactly what the
//! environment owns.
//!
//! An environment's trace visits what its value captures own and the cells of its captured
//! variables, each a counted `[count][value]` block traced like a weak map box. Borrowed
//! captures belong to the frame that made the closure and are not visited.

use velt_sema::hir::{DefId, PassMode, TyId, TyKind};

use crate::lower::boxing::{Boxing, CycleKey};
use crate::lower::closure::closure_name;
use crate::lower::operand::proj;
use crate::lower::{cfunc, unit, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

use super::Glue;

impl Cx<'_> {
    /// The closures whose environments a function value of type `t` may hold and the trace
    /// glue looks into (`b`: the counted types; `None`: this pass's).
    pub(in crate::lower) fn traced_closures(
        &mut self,
        b: Option<&Boxing>,
        t: TyId,
    ) -> Vec<(DefId, Vec<TyId>)> {
        if !matches!(self.kind(t), TyKind::FnPtr { .. }) {
            return vec![];
        }
        let Some(key) = self.cycle_key(t) else {
            return vec![];
        };
        let bx = b.unwrap_or(&self.boxing);
        bx.env_traced.get(&key).cloned().unwrap_or_default()
    }

    /// What the environment of closure `def` at `targs` owns: each capture that is copied or
    /// moved in, as its type and whether it is a captured variable's cell. These are the
    /// captures the environment's drop releases and its trace glue visits (`value_captures`
    /// and `cell_captures` with the same modes); a borrowed capture, cell or not, belongs to
    /// the frame that made the closure. A `weak` variable's capture holds a slot, no edge.
    pub(in crate::lower) fn env_owned(&mut self, def: DefId, targs: &[TyId]) -> Vec<(TyId, bool)> {
        let f = self.fn_def(def);
        let caps: Vec<(TyId, bool)> = (f.captures.iter())
            .filter(|c| matches!(c.mode, PassMode::Copy | PassMode::Owned))
            .filter(|c| !f.body.locals[c.inner.0 as usize].weak)
            .map(|c| {
                let l = &f.body.locals[c.inner.0 as usize];
                (l.ty, l.boxed)
            })
            .collect();
        (caps.into_iter())
            .map(|(t, cell)| (self.subst(t, targs), cell))
            .collect()
    }

    /// The closures of the program by the key of the function type they were made at, from
    /// this pass's facts (sorted, so the fixed point compares equal across passes).
    pub(in crate::lower) fn closures_by_key(
        &self,
    ) -> rustc_hash::FxHashMap<CycleKey, Vec<(DefId, Vec<TyId>)>> {
        let mut out: rustc_hash::FxHashMap<CycleKey, Vec<(DefId, Vec<TyId>)>> = Default::default();
        for (key, def, targs) in self.facts.closure_envs.iter() {
            if let Some(k) = key {
                out.entry(k.clone())
                    .or_default()
                    .push((*def, targs.clone()));
            }
        }
        for v in out.values_mut() {
            v.sort_by_key(|(d, targs)| (d.0, targs.iter().map(|t| t.0).collect::<Vec<_>>()));
            v.dedup();
        }
        out
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// [`Work::EnvTrace`] (module docs).
    pub(in crate::lower) fn build_env_trace(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let visit = lw.new_local(Ty::Ptr, Some("visit".into()));
        let ctx = lw.new_local(Ty::Ptr, Some("ctx".into()));
        let ea = lw.cx.env_agg(def, targs);
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(ea)));
        let tv = (visit, ctx);
        for (field, mode, ty) in lw.value_captures(def) {
            if matches!(mode, PassMode::Copy | PassMode::Owned) {
                lw.trace_part(proj(&base, Proj::Field(field)), ty, tv);
            }
        }
        for (field, ty) in lw.cell_captures(def) {
            if !lw.cx.weak_refs_inline(None, ty).is_empty() {
                let cell = Operand::Copy(proj(&base, Proj::Field(field)));
                let glue = cfunc(lw.cx.func(Work::Glue(Glue::WeakBoxTrace, ty)));
                lw.visit_child(cell, glue, tv);
            }
        }
        lw.terminate(Terminator::Return(unit()));
        let sym = format!(
            "_Genv_trace_{}",
            lw.cx.instance_symbol(&closure_name(lw.cx.hir, def), targs)
        );
        lw.finish(sym, vec![Ty::Ptr; 3], Ty::Unit)
    }

    /// The function value of type `t` at `place`: visit its environment with the trace glue of
    /// the closure whose drop entry it has (module docs).
    pub(super) fn trace_fn_value(&mut self, place: &Place, t: TyId, tv: (vir::Local, vir::Local)) {
        let closures = self.cx.traced_closures(None, t);
        let closures: Vec<(DefId, Vec<TyId>)> = (closures.into_iter())
            .filter(|(d, targs)| self.cx.env_refers(*d, targs))
            .collect();
        if closures.is_empty() {
            return;
        }
        let (env, _) = self.closure_env(place);
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let hdr = self.cx.closure_agg();
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let drop = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(
                &proj(&ep, Proj::Deref(Ty::Agg(hdr))),
                Proj::Field(0),
            ))),
        );
        for (def, targs) in closures {
            let want = cfunc(self.cx.func(Work::EnvDrop(def, targs.clone())));
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, drop.clone(), want));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(is, yes, no);
            self.switch_to(yes);
            let glue = cfunc(self.cx.func(Work::EnvTrace(def, targs)));
            self.visit_child(env.clone(), glue, tv);
            self.goto(done);
            self.switch_to(no);
        }
        self.goto(done);
        self.switch_to(done);
    }

    /// `visit(ctx, child, glue)` (glue/trace.rs `call_visit`).
    fn visit_child(&mut self, child: Operand, glue: Operand, tv: (vir::Local, vir::Local)) {
        self.call_visit(tv, child, glue);
    }
}

impl Cx<'_> {
    /// Does the environment of closure `def` at `targs` own a counted object the trace glue
    /// visits? One that owns none can't be on a path back to a key, so it isn't visited.
    pub(in crate::lower) fn env_refers(&mut self, def: DefId, targs: &[TyId]) -> bool {
        let owned = self.env_owned(def, targs);
        owned
            .into_iter()
            .any(|(t, _)| !self.weak_refs_inline(None, t).is_empty())
    }
}
