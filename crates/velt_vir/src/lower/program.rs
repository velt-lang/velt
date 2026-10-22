//! Program-level lowering state: interning of externs and static data, and the worklist of
//! functions to build ([`Work`] → [`FuncId`], each built exactly once, in request order).

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;

use velt_sema::hir::{self, DefId};

use super::layout::Layouts;
use super::rt::Rt;
use super::{ice, Cx, Work};
use crate::vir::Ty;
use crate::vir::{self, AggLayout, ExternFn, ExternId, FuncId, Function, StaticData, StaticId};

impl<'h> Cx<'h> {
    /// A pass over `hir` with the type table `types` (the HIR's, plus what earlier passes
    /// interned) and the counted types `boxing`.
    pub(super) fn new(
        hir: &'h hir::Program,
        types: hir::TyTable,
        boxing: super::boxing::Boxing,
    ) -> Self {
        Cx {
            hir,
            native_inits: vec![],
            uses_placement: false,
            cell_checks: false,
            no_await_steps: false,
            types,
            aggs: vec![AggLayout {
                name: "string".into(),
                size: 24,
                align: 8,
                fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
            }],
            externs: vec![],
            extern_map: FxHashMap::default(),
            statics: vec![],
            module_desc: None,
            reassigned_slots: None,
            prelude_error: None,
            static_map: FxHashMap::default(),
            tables: FxHashMap::default(),
            funcs: vec![],
            work_map: FxHashMap::default(),
            queue: VecDeque::new(),
            building: FxHashSet::default(),
            asyncs: FxHashMap::default(),
            by_ref_params: FxHashSet::default(),
            shared_envs: FxHashMap::default(),
            lay: Layouts::default(),
            locs: None,
            debug: None,
            tracked: FxHashMap::default(),
            quiet: FxHashMap::default(),
            str_objects: FxHashMap::default(),
            symbol_records: Default::default(),
            boxing,
            facts: Default::default(),
            iface_impls: None,
            own_clones: None,
            honoured_clones: FxHashMap::default(),
            dyn_modes_memo: FxHashMap::default(),
            json_key_memo: None,
            anon: Default::default(),
            cur_def: None,
            nothing_clashes: vec![],
            map_fields: 0,
        }
    }

    /// Errors for `nothing_clashes`, one per function.
    pub(super) fn nothing_clash_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        let mut seen = FxHashSet::default();
        let mut out = vec![];
        for d in &self.nothing_clashes {
            if !seen.insert(*d) {
                continue;
            }
            let (name, span) = match d.map(|d| self.hir.def(d)) {
                Some(hir::Def::Fn(f)) => (f.name.rsplit("::").next().unwrap_or(&f.name), f.span),
                _ => ("the program", velt_common::Span::DUMMY),
            };
            out.push(
                velt_common::Diagnostic::error(
                    format!("an instance of `{name}` would hold both `null` and `undefined` in one value, which is not supported yet (#930)"),
                    span,
                )
                .with_note("a type argument with one nothing (`T | undefined`) meets an option with the other (`T | null`) in this code"),
            );
        }
        out
    }

    /// Queue the user entry (`velt_main` is queued once everything it reaches is lowered:
    /// `lower_program`). Everything else (callees, generic instances, glue, thunks, vtables) is
    /// queued on demand by `func`, so unused functions are never lowered.
    pub(super) fn seed_functions(&mut self) {
        // An async main is reached through its poll function (entry.rs), not its constructor.
        match self.hir.entry {
            Some(e) if self.is_async_fn(e) => self.func(Work::Poll(e, vec![])),
            Some(e) => self.func(Work::Fn(e, vec![])),
            None => return,
        };
    }

    pub(super) fn finish(mut self) -> vir::Program {
        self.fill_registry();
        let mut funcs: Vec<Function> = self
            .funcs
            .into_iter()
            .map(|f| f.unwrap_or_else(|| ice("function never lowered")))
            .collect();
        super::keys::disambiguate(&mut funcs);
        vir::Program {
            aggs: self.aggs,
            funcs,
            externs: self.externs,
            statics: self.statics,
            files: self.locs.map(|m| m.files).unwrap_or_default(),
            debug_types: self.debug.map(|d| d.types).unwrap_or_default(),
        }
    }

    /// The function built for `work`; queued for building on first request.
    pub(super) fn func(&mut self, work: Work) -> FuncId {
        if let Some(&id) = self.work_map.get(&work) {
            return id;
        }
        let id = FuncId(self.funcs.len() as u32);
        self.funcs.push(None);
        self.work_map.insert(work.clone(), id);
        self.queue.push_back((id, work));
        id
    }

    /// Build the function for `work` now unless it is already built or being built (in which
    /// case the caller sees no result: recursion through eagerly built poll functions).
    pub(super) fn build_now(&mut self, fid: FuncId, work: &Work) {
        if self.funcs[fid.0 as usize].is_some() || !self.building.insert(fid) {
            return;
        }
        let outer = self.cur_def;
        self.cur_def = work_def(work).or(outer);
        let func = super::FnLower::build(self, work);
        self.cur_def = outer;
        self.funcs[fid.0 as usize] = Some(func);
        self.building.remove(&fid);
    }

    /// Instance of a user function with concrete type args.
    pub(super) fn func_for(&mut self, def: DefId, targs: Vec<hir::TyId>) -> FuncId {
        self.func(Work::Fn(def, targs))
    }

    /// Whether the code built so far starts promises or tasks (a synchronous `main` then runs as a
    /// task, entry.rs).
    pub(super) fn reaches_tasks(&self) -> bool {
        self.extern_map
            .keys()
            .any(|s| s.starts_with("velt_rt_fut_start") || s.starts_with("velt_rt_spawn"))
    }

    pub(super) fn rt(&mut self, r: Rt) -> ExternId {
        let (sym, params, ret, noreturn) = r.sig();
        self.extern_sym(sym, params, ret, noreturn)
    }

    /// Intern an extern by symbol; the same symbol must always have the same signature.
    pub(super) fn extern_sym(
        &mut self,
        symbol: &str,
        params: Vec<Ty>,
        ret: Ty,
        noreturn: bool,
    ) -> ExternId {
        // Only pinning selects the per-core scheduler: `workerCount()` sizes pools of unpinned
        // tasks, which run faster on the stealing one.
        if symbol == "velt_rt_spawn_target" {
            self.uses_placement = true;
        }
        if let Some(&id) = self.extern_map.get(symbol) {
            let e = &self.externs[id.0 as usize];
            if e.params != params || e.ret != ret {
                ice(format_args!(
                    "extern `{symbol}` declared with two different signatures"
                ));
            }
            return id;
        }
        let id = ExternId(self.externs.len() as u32);
        self.externs.push(ExternFn {
            symbol: symbol.to_string(),
            params,
            ret,
            noreturn,
        });
        self.extern_map.insert(symbol.to_string(), id);
        id
    }

    pub(super) fn static_bytes(&mut self, bytes: Vec<u8>, align: u32) -> StaticId {
        let key = (bytes, align);
        if let Some(&id) = self.static_map.get(&key) {
            return id;
        }
        let id = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes: key.0.clone(),
            align,
            relocs: vec![],
        });
        self.static_map.insert(key, id);
        id
    }

    /// A read-only `VeltStr` object (not just its bytes) holding `s`, for pointers that must
    /// outlive any stack frame (e.g. the throw location kept by the runtime). Memoized.
    pub(super) fn static_str_object(&mut self, s: &str) -> StaticId {
        if let Some(&id) = self.str_objects.get(s) {
            return id;
        }
        let mut bytes = s.as_bytes().to_vec();
        if bytes.is_empty() {
            bytes.push(0);
        }
        let text = self.static_bytes(bytes, 1);
        let mut object = vec![0u8; 24];
        object[8..16].copy_from_slice(&super::strings::str_w1(s).to_le_bytes());
        let id = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes: object,
            align: 8,
            relocs: vec![(0, vir::Const::Static(text))],
        });
        self.str_objects.insert(s.to_string(), id);
        id
    }
}

/// The function `work` builds an instance (or a part) of.
fn work_def(work: &Work) -> Option<DefId> {
    match work {
        Work::Fn(d, _)
        | Work::Thunk(_, d, _)
        | Work::EnvDrop(d, _)
        | Work::EnvDropFrame(d, _)
        | Work::EnvClone(d, _)
        | Work::EnvTransfer(d, _)
        | Work::Tracked(d, _, _)
        | Work::Frozen(d, _, _, _, _)
        | Work::Poll(d, _)
        | Work::AsyncDrop(d, _)
        | Work::AsyncCloseStart(d, _)
        | Work::GenThrow(d, _)
        | Work::ValuePoll(d, _)
        | Work::ValueDrop(d, _)
        | Work::HandlerInit(d, _)
        | Work::GenNew(d, _)
        | Work::GenFree(d, _) => Some(*d),
        _ => None,
    }
}
