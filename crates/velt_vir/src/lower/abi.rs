//! Function construction: dispatches each [`Work`] item to its builder, and for user functions
//! applies the calling convention documented in lib.rs (aggregate params by pointer, trailing
//! out-pointer for aggregate or `Result` returns, hidden env param for closures), allocates VIR
//! locals for HIR locals and drop flags, then lowers the body.

use std::collections::HashSet;

use velt_sema::hir::{self, DefId, FnDef, PassMode, TyId, TyKind};

use super::flags::FlagScan;
use super::param_attrs::PtrParam;
use super::{ice, Cx, FnLower, LInfo, LState, ScopeKind, Work};
use crate::vir::{BlockId, Const, Function, Operand, ParamAttrs, Place, Proj, Rvalue, SrcLoc, Ty};

/// ABI shape of a function's result.
#[derive(Clone, Copy, Debug)]
pub(super) struct RetAbi {
    /// Type of the trailing out-pointer's pointee (aggregate or `Result` return).
    pub out: Option<Ty>,
    /// VIR return type (`Unit` when there is an out-pointer).
    pub ret: Ty,
}

impl Cx<'_> {
    /// Result ABI of a function returning `ret` and possibly throwing `throws`.
    pub(super) fn ret_abi(&mut self, ret: TyId, throws: Option<TyId>) -> RetAbi {
        let vt = match throws {
            Some(e) => {
                let r = self.intern(TyKind::Result(ret, e));
                self.ty(r)
            }
            None => self.ty(ret),
        };
        match vt {
            Ty::Agg(_) => RetAbi {
                out: Some(vt),
                ret: Ty::Unit,
            },
            t => RetAbi { out: None, ret: t },
        }
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// An empty function builder with an entry block (for user functions and glue alike).
    pub(super) fn bare(cx: &'c mut Cx<'h>, targs: Vec<TyId>) -> Self {
        let mut lw = FnLower {
            cx,
            targs,
            locals: vec![],
            blocks: vec![],
            block_locs: vec![],
            loc: None,
            caller_loc: None,
            live: vec![],
            cur: BlockId(0),
            info: vec![],
            scopes: vec![],
            out_ptr: None,
            ret_ty: None,
            throws: None,
            div_zero_bbs: vec![],
            ref_bindings: HashSet::new(),
            asyncx: None,
            lazy_call: false,
            retain_hops: false,
            pending_borrows: vec![],
            share_binds: false,
            transfer_args: false,
            transfer_call: false,
            same_mode: false,
            key_mode: false,
            ctor_self: None,
            init_stack: vec![],
        };
        let entry = lw.new_block();
        lw.live[entry.0 as usize] = true;
        lw
    }

    pub(super) fn build(cx: &'c mut Cx<'h>, work: &Work) -> Function {
        match work {
            Work::Fn(def, targs) => Self::lower_fn(cx, *def, targs, None),
            Work::Tracked(def, targs, at) => Self::lower_fn(cx, *def, targs, Some(*at)),
            Work::Glue(g, t) => Self::build_glue(cx, *g, *t),
            Work::Thunk(kind, def, targs) => Self::build_thunk(cx, *kind, *def, targs),
            Work::EnvDrop(def, targs) => Self::build_env_drop(cx, *def, targs),
            Work::EnvClone(def, targs) => Self::build_env_clone(cx, *def, targs),
            Work::EnvTransfer(def, targs) => Self::build_env_transfer(cx, *def, targs),
            Work::Oob(signed) => Self::build_oob(cx, *signed),
            Work::ArrayGrow => Self::build_array_grow(cx),
            Work::Main => Self::build_main(cx),
            Work::Poll(def, targs) => Self::build_poll(cx, *def, targs),
            Work::AsyncDrop(def, targs) => Self::build_async_drop(cx, *def, targs),
            Work::AsyncCloseStart(def, targs) => Self::build_close_start(cx, *def, targs),
            Work::ValuePoll(def, targs) => Self::build_value_poll(cx, *def, targs),
            Work::ValueDrop(def, targs) => Self::build_value_drop(cx, *def, targs),
            Work::AllPoll(t) => Self::build_all_poll(cx, *t),
            Work::AllDrop(t) => Self::build_all_drop(cx, *t),
            Work::RaceBoxPoll(t) => Self::build_race_box_poll(cx, *t),
            Work::RaceBoxDrop(t) => Self::build_race_box_drop(cx, *t),
            Work::WidenPoll(from, to) => Self::build_widen_poll(cx, *from, *to),
            Work::WidenDrop(from, to) => Self::build_widen_drop(cx, *from, *to),
            Work::HandlerInit(def, targs) => Self::build_handler_init(cx, *def, targs),
            Work::Unclaimed(t) => Self::build_unclaimed(cx, *t),
            Work::Init(t, e) => Self::build_init(cx, *t, *e),
            Work::GenNew(def, targs) => Self::build_gen_new(cx, *def, targs),
            Work::GenFree(def, targs) => Self::build_gen_free(cx, *def, targs),
        }
    }

    /// Substitute the current type arguments into a HIR type.
    pub(super) fn sub(&mut self, t: TyId) -> TyId {
        let targs = std::mem::take(&mut self.targs);
        let r = self.cx.subst(t, &targs);
        self.targs = targs;
        r
    }

    /// VIR type of a HIR type in the current instance.
    pub(super) fn vty(&mut self, t: TyId) -> Ty {
        let t = self.sub(t);
        self.cx.ty(t)
    }

    pub(super) fn needs_drop(&mut self, t: TyId) -> bool {
        let t = self.sub(t);
        self.cx.needs_drop(t)
    }

    pub(super) fn kind(&mut self, t: TyId) -> TyKind {
        let t = self.sub(t);
        self.cx.kind(t)
    }

    /// A user function instance; `caller` is the call site of a caller-tracking instance.
    fn lower_fn(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
        caller: Option<SrcLoc>,
    ) -> Function {
        let hir_prog = cx.hir;
        let hir::Def::Fn(f) = hir_prog.def(def) else {
            ice("function instance is not a Def::Fn")
        };
        if f.is_generator {
            return Self::build_gen_fn(cx, def, targs);
        }
        if f.is_async {
            return Self::build_async_new(cx, def, targs);
        }
        let mut lw = FnLower::bare(cx, targs.to_vec());
        lw.enter_span(f.span);
        lw.caller_loc = caller;
        let ret = lw.sub(f.ret);
        lw.ret_ty = Some(ret);
        lw.throws = lw.cx.fn_throws(f, targs);
        lw.ctor_self = lw.ctor_class(def, f);
        let scan = FlagScan::run(hir_prog, &f.body);
        lw.ref_bindings = scan.ref_bindings.iter().copied().collect();
        let (params, attrs) = lw.declare_locals(def, f);
        lw.declare_drop_flags(f, &scan);
        if !lw.cx.by_ref_params.contains(&(def, lw.targs.clone())) {
            for p in &f.params {
                lw.copy_modified_param(f, p);
            }
        }
        let abi = lw.cx.ret_abi(ret, lw.throws);
        lw.lower_body(f);
        let mut symbol = lw.cx.instance_symbol(&f.name, targs);
        if let Some(at) = caller {
            // `_L` never follows a mangled name or `_T` list, so instances stay distinct.
            symbol.push_str(&format!("_L{}_{}_{}", at.file, at.line, at.col));
        }
        let mut func = lw.finish(symbol, params, abi.ret);
        func.param_attrs = attrs;
        func
    }

    /// Env param (closures), params in HIR order, the out-pointer, then all other HIR locals.
    /// Returns the VIR param types and their attributes (param_attrs.rs).
    fn declare_locals(&mut self, def: DefId, f: &FnDef) -> (Vec<Ty>, Vec<ParamAttrs>) {
        let mut info: Vec<Option<LInfo>> = f.body.locals.iter().map(|_| None).collect();
        let mut params = vec![];
        let mut attrs = vec![];
        let env = (!f.captures.is_empty()).then(|| {
            params.push(Ty::Ptr);
            attrs.push(ParamAttrs::default());
            self.new_local(Ty::Ptr, Some("env".into()))
        });
        let captured: HashSet<_> = f.captures.iter().map(|c| c.inner).collect();
        let by_ref = self.cx.by_ref_params.contains(&(def, self.targs.clone()));
        for p in &f.params {
            if captured.contains(&p.local) {
                continue;
            }
            let decl = &f.body.locals[p.local.0 as usize];
            let ty = self.sub(p.ty);
            let vt = self.cx.ty(ty);
            let indirect = match vt {
                Ty::Unit => false,
                Ty::Agg(_) => true,
                _ => by_ref,
            };
            let vir = (vt != Ty::Unit).then(|| {
                let pt = if indirect { Ty::Ptr } else { vt };
                params.push(pt);
                // Every class-typed param points to its object (not only `this`): sema's
                // exclusivity rule covers all params alike (param_attrs.rs).
                let shape = match (indirect, self.cx.is_class(ty)) {
                    (true, _) => PtrParam::Value(vt),
                    (false, true) => PtrParam::Object,
                    (false, false) => PtrParam::NotPtr,
                };
                // `Mutex.with` callbacks write the locked value through a by-reference param
                // whatever its mode; a param the body modifies (`mutable`) is never read-only.
                let mode = match by_ref && !matches!(vt, Ty::Agg(_)) {
                    true => None,
                    false => Some(p.mode),
                };
                attrs.push(self.cx.param_attrs(mode, ty, shape, decl.mutable));
                self.new_local(pt, Some(decl.name.clone()))
            });
            let droppable = p.mode == PassMode::Owned && self.cx.needs_drop(ty);
            info[p.local.0 as usize] = Some(LInfo::new(vir, ty, indirect, droppable, LState::Init));
        }
        let abi = self
            .cx
            .ret_abi(self.ret_ty.unwrap_or_else(|| ice("ret")), self.throws);
        if let Some(out) = abi.out {
            self.out_ptr = Some(self.new_local(Ty::Ptr, Some("ret.out".into())));
            params.push(Ty::Ptr);
            attrs.push(self.cx.out_ptr_attrs(out));
        }
        if let Some(env) = env {
            self.bind_captures(def, f, env, &mut info);
        }
        self.declare_body_locals(f, info);
        (params, attrs)
    }

    /// Every HIR local not declared yet (params, captures): a VIR local of its value type, or a
    /// pointer for bindings by reference. Installs the complete `info` table.
    pub(super) fn declare_body_locals(&mut self, f: &FnDef, mut info: Vec<Option<LInfo>>) {
        for (i, ld) in f.body.locals.iter().enumerate() {
            if info[i].is_none() {
                let ty = self.sub(ld.ty);
                let by_ref = self.ref_bindings.contains(&hir::LocalId(i as u32));
                let t = self.cx.ty(ty);
                let cell = ld.boxed && t != Ty::Unit;
                let (vt, indirect) = match t {
                    _ if cell => (Ty::Ptr, true),
                    Ty::Agg(_) if by_ref => (Ty::Ptr, true),
                    t => (t, false),
                };
                let vir = (vt != Ty::Unit).then(|| self.new_local(vt, Some(ld.name.clone())));
                let droppable = cell || vir.is_some() && !by_ref && self.cx.needs_drop(ty);
                let mut li = LInfo::new(vir, ty, indirect, droppable, LState::Uninit);
                li.cell = cell;
                li.in_cell = cell;
                info[i] = Some(li);
            }
        }
        self.info = info
            .into_iter()
            .map(|i| i.unwrap_or_else(|| ice("undeclared local")))
            .collect();
    }

    /// Allocate drop flags (initialized to `false` in the entry block so they are always defined)
    /// and record which locals zero their moved-out fields (`LInfo::zero_parts`).
    pub(super) fn declare_drop_flags(&mut self, f: &FnDef, scan: &super::flags::Scan) {
        for (i, &zero) in scan.zero_parts.iter().enumerate() {
            self.info[i].zero_parts = zero;
        }
        for (i, &needs_flag) in scan.flagged.iter().enumerate() {
            if self.info[i].droppable && needs_flag {
                let name = format!("{}.dropflag", f.body.locals[i].name);
                let fl = self.new_local(Ty::Bool, Some(name));
                self.info[i].flag = Some(fl);
                self.assign(
                    Place::local(fl),
                    Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
                );
            }
        }
    }

    pub(super) fn lower_body(&mut self, f: &FnDef) {
        // Outermost scope: owned params, dropped on every return.
        self.push_scope(ScopeKind::Block);
        for p in &f.params[f.captures.len()..] {
            self.box_param(f, p);
        }
        for p in &f.params {
            if self.info[p.local.0 as usize].droppable {
                self.mark_init(p.local);
                self.register_local_drop(p.local);
            }
        }
        if self.asyncx.is_some() {
            self.cancel_before_start(f);
        }
        self.push_scope(ScopeKind::Block);
        self.ctor_entry_inits();
        let unit_super = self.unit_super_at(f);
        for (i, s) in f.body.block.stmts.iter().enumerate() {
            self.stmt(s);
            if unit_super == Some(i) {
                self.unit_super_inits();
            }
        }
        // A generator's body yields its values and returns nothing.
        let returns_value = !self.returns_unit() && !self.in_generator();
        if let Some(v) = &f.body.block.value {
            // A trailing value expression of a function body is its return value.
            self.push_scope(ScopeKind::Temps);
            if returns_value {
                let op = self.consume(v);
                self.emit_return(Some(op));
            } else {
                self.expr(v);
            }
            self.pop_scope();
        }
        if returns_value {
            // Sema guarantees non-void functions return on every path.
            self.terminate(crate::vir::Terminator::Unreachable);
        } else {
            self.emit_return(None);
        }
        self.pop_scope();
        self.pop_scope();
    }

    /// A Copy aggregate param the body modifies gets its own copy: calls through the borrow ABI
    /// (function values, vtables) pass a pointer to the caller's value, which must not change.
    /// (Not for `Mutex.with` callbacks, which write the locked value on purpose.)
    fn copy_modified_param(&mut self, f: &FnDef, p: &hir::Param) {
        let l = p.local.0 as usize;
        let info = &self.info[l];
        let (Some(ptr), true) = (info.vir, info.indirect) else {
            return;
        };
        if p.mode != PassMode::Copy || !f.body.locals[l].mutable {
            return;
        }
        let vt = self.cx.ty(info.ty);
        let copy = self.new_local(vt, Some(f.body.locals[l].name.clone()));
        let src = Place {
            local: ptr,
            proj: vec![Proj::Deref(vt)],
        };
        self.assign(Place::local(copy), Rvalue::Use(Operand::Copy(src)));
        self.info[l].vir = Some(copy);
        self.info[l].indirect = false;
    }

    /// The function's declared result is `void`/`never` (possibly wrapped in a `Result`).
    fn returns_unit(&mut self) -> bool {
        let r = self.ret_ty.unwrap_or_else(|| ice("ret"));
        self.cx.is_unit(r)
    }

    /// Place of the value written through the out-pointer (the Ok payload for throwing fns).
    pub(super) fn out_place(&mut self) -> Place {
        let out = self.out_ptr.unwrap_or_else(|| ice("no out pointer"));
        let ret = self.ret_ty.unwrap_or_else(|| ice("ret"));
        match self.throws {
            Some(e) => {
                let r = self.cx.intern(TyKind::Result(ret, e));
                let rv = self.cx.ty(r);
                let view = self.cx.view(r, 0);
                Place {
                    local: out,
                    proj: vec![Proj::Deref(rv), Proj::Cast(view), Proj::Field(1)],
                }
            }
            None => {
                let rv = self.cx.ty(ret);
                Place {
                    local: out,
                    proj: vec![Proj::Deref(rv)],
                }
            }
        }
    }
}

impl LInfo {
    pub(super) fn new(
        vir: Option<crate::vir::Local>,
        ty: TyId,
        indirect: bool,
        droppable: bool,
        state: LState,
    ) -> Self {
        LInfo {
            vir,
            ty,
            indirect,
            droppable,
            flag: None,
            state,
            moved_fields: vec![],
            zero_parts: false,
            cell: false,
            in_cell: false,
            gen: None,
        }
    }
}
