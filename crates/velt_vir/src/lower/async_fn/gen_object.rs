//! Generator objects (generator.rs module docs): the prelude class `Generator<T, E>` whose one
//! field (`state: u64`) is the table pointer, followed by the generator's state. A generator
//! instance `def<targs>` has
//! - a static table `[resume, close, free]` (`f$poll`, `f$drop`, [`Work::GenFree`]);
//! - [`Work::GenNew`]: its constructor with the ordinary calling convention, returning the
//!   object (one allocation, counted when `Generator<T, E>` is);
//! - [`Work::Fn`]: what a call returns — the object, or an interface value of it when the
//!   generator is declared to return `Iterator<T, E>` / `Iterable<T, E>`;
//! - [`Work::GenFree`] `(obj)`: frees the object's block (its state was closed by the class's
//!   `[Symbol.dispose]`, which the object's drop runs first).

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::generator::GEN_STATE_OFF;
use super::AsyncInfo;
use crate::lower::glue::VtableKey;
use crate::lower::operand::proj;
use crate::lower::{ice, Cx, FnLower, Work};
use crate::vir::{
    self, AggId, AggLayout, Const, Function, Local, Operand, Place, Proj, Rvalue, StaticData,
    StaticId, Terminator, Ty,
};

/// Qualified name of the prelude's generator class (std/prelude/iter.vlt).
const PRELUDE_GENERATOR: &str = "std/prelude/iter::Generator";

impl Cx<'_> {
    /// Is `t` the prelude's `Generator<T, E>` class?
    pub(in crate::lower) fn is_generator_obj(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) => {
                matches!(self.hir.def(*d), hir::Def::Adt(a) if a.name == PRELUDE_GENERATOR)
            }
            _ => false,
        }
    }

    /// `Generator<T, E>` (the class type) of generator instance `def<targs>`.
    pub(in crate::lower) fn gen_class_ty(&mut self, def: DefId, targs: &[TyId]) -> TyId {
        let f = self.fn_def(def);
        let (t, e) = self.gen_args(f, targs);
        let d = (0..self.hir.defs.len() as u32)
            .map(DefId)
            .find(|&d| matches!(self.hir.def(d), hir::Def::Adt(a) if a.name == PRELUDE_GENERATOR))
            .unwrap_or_else(|| ice("the prelude has no `Generator` class"));
        self.intern(TyKind::Adt(d, vec![t, e]))
    }

    /// The object layout of instance `def<targs>`: `{ table: ptr, state }`.
    fn gen_box(&mut self, info: &AsyncInfo, name: &str) -> AggId {
        let (size, align) = self.size_align(Ty::Agg(info.state));
        if i128::from(align) > GEN_STATE_OFF {
            ice("generator state aligned beyond its table pointer");
        }
        let off = GEN_STATE_OFF as u32;
        self.push_agg(AggLayout {
            name: format!("{name} generator"),
            size: (off + size).next_multiple_of(8),
            align: 8,
            fields: vec![(Ty::Ptr, 0), (Ty::Agg(info.state), off)],
        })
    }

    /// The table of instance `def<targs>` (built once per lowering pass).
    fn gen_table(&mut self, def: DefId, targs: &[TyId], info: &AsyncInfo) -> StaticId {
        if let Some(&s) = self.lay.gen_tables.get(&(def, targs.to_vec())) {
            return s;
        }
        let close = self.func(Work::AsyncDrop(def, targs.to_vec()));
        let free = self.func(Work::GenFree(def, targs.to_vec()));
        let relocs = vec![
            (0, Const::Func(info.poll)),
            (8, Const::Func(close)),
            (16, Const::Func(free)),
        ];
        let bytes = vec![0; 24];
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
        });
        let id = StaticId(self.statics.len() as u32 - 1);
        self.lay.gen_tables.insert((def, targs.to_vec()), id);
        id
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// [`Work::GenNew`] (module docs).
    pub(in crate::lower) fn build_gen_new(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let (mut lw, params, info, s, _) = Self::ctor_state(cx, def, targs, false);
        let obj = lw.gen_object(def, targs, &info, s);
        lw.terminate(Terminator::Return(obj));
        let sym = format!(
            "{}$new",
            lw.cx.instance_symbol(&lw.cx.fn_def(def).name, targs)
        );
        lw.finish(sym, params, Ty::Ptr)
    }

    /// [`Work::Fn`] of a generator (module docs).
    pub(in crate::lower) fn build_gen_fn(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let f = cx.fn_def(def);
        let ret = cx.subst(f.ret, targs);
        let dyn_ret = matches!(cx.kind(ret), TyKind::Dyn(..));
        let (mut lw, mut params, info, s, out) = Self::ctor_state(cx, def, targs, dyn_ret);
        let obj = lw.gen_object(def, targs, &info, s);
        let sym = lw.cx.instance_symbol(&f.name, targs);
        let Some(out) = out else {
            lw.terminate(Terminator::Return(obj));
            return lw.finish(sym, params, Ty::Ptr);
        };
        let TyKind::Dyn(iface, iargs) = lw.cx.kind(ret) else {
            ice("generator interface result")
        };
        let gty = lw.cx.gen_class_ty(def, targs);
        let index = lw.cx.find_impl(iface, &iargs, gty);
        let vtable = lw.vtable_addr(VtableKey::Impl(index, gty));
        let Ty::Agg(a) = lw.cx.ty(ret) else {
            ice("interface value is not an aggregate")
        };
        let dst = proj(&Place::local(out), Proj::Deref(Ty::Agg(a)));
        lw.assign(dst, Rvalue::Aggregate(a, vec![obj, vtable]));
        lw.terminate(Terminator::Return(crate::lower::unit()));
        params.push(Ty::Ptr);
        lw.finish(sym, params, Ty::Unit)
    }

    /// A new generator object holding the initial state in local `s`.
    fn gen_object(&mut self, def: DefId, targs: &[TyId], info: &AsyncInfo, s: Local) -> Operand {
        let name = self.cx.fn_def(def).name.clone();
        let bx = self.cx.gen_box(info, &name);
        let gty = self.cx.gen_class_ty(def, targs);
        let obj = if self.cx.counted(gty) {
            self.counted_alloc(Ty::Agg(bx))
        } else {
            self.alloc(Ty::Agg(bx))
        };
        let obj = self.copy_to_temp(obj, Ty::Ptr);
        let base = proj(&Place::local(obj), Proj::Deref(Ty::Agg(bx)));
        let table = self.cx.gen_table(def, targs, info);
        self.assign(
            proj(&base, Proj::Field(0)),
            Rvalue::Use(Operand::Const(Const::Static(table), Ty::Ptr)),
        );
        self.assign(
            proj(&base, Proj::Field(1)),
            Rvalue::Use(Operand::Copy(Place::local(s))),
        );
        Operand::Copy(Place::local(obj))
    }

    /// [`Work::GenFree`] (module docs).
    pub(in crate::lower) fn build_gen_free(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let info = cx
            .async_info(def, targs)
            .unwrap_or_else(|| ice("generator state layout unavailable"));
        let name = cx.fn_def(def).name.clone();
        let bx = cx.gen_box(&info, &name);
        let gty = cx.gen_class_ty(def, targs);
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let obj = lw.new_local(Ty::Ptr, Some("obj".into()));
        let o = Operand::Copy(Place::local(obj));
        if lw.cx.counted(gty) {
            lw.counted_free(o, Ty::Agg(bx));
        } else {
            lw.free(o, Ty::Agg(bx));
        }
        lw.terminate(Terminator::Return(crate::lower::unit()));
        let sym = format!("{}$free", lw.cx.instance_symbol(&name, targs));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }

    /// Free generator object `ptr` (its state already closed): through its table, which knows
    /// the instance's size.
    pub(in crate::lower) fn gen_object_free(&mut self, ptr: Operand) {
        let f = self.gen_table_entry(ptr.clone(), super::generator::TABLE_FREE);
        self.call_entry(f, vec![ptr], vec![Ty::Ptr], Ty::Unit);
    }

    /// A call of generator `def<targs>(args)` creating its object (not the declared interface
    /// value): [`Work::GenNew`].
    pub(in crate::lower) fn gen_new_call(
        &mut self,
        def: DefId,
        targs: Vec<TyId>,
        args: &[hir::Expr],
    ) -> Operand {
        let f = self.cx.fn_def(def);
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        let fid = self.cx.func(Work::GenNew(def, targs.clone()));
        let argv = self.lower_args(args, &modes, true);
        let gty = self.cx.gen_class_ty(def, &targs);
        self.finish_call(vir::Callee::Func(fid), argv, gty, None)
    }
}
