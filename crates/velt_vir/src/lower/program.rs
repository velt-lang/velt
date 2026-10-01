//! Program-level lowering state: interning of externs and static data, and the worklist of
//! functions to build ([`Work`] → [`FuncId`], each built exactly once, in request order).

use std::collections::{HashMap, HashSet, VecDeque};

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
            types,
            aggs: vec![AggLayout {
                name: "string".into(),
                size: 24,
                align: 8,
                fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
            }],
            externs: vec![],
            extern_map: HashMap::new(),
            statics: vec![],
            static_map: HashMap::new(),
            funcs: vec![],
            work_map: HashMap::new(),
            queue: VecDeque::new(),
            building: HashSet::new(),
            asyncs: HashMap::new(),
            by_ref_params: HashSet::new(),
            shared_envs: HashMap::new(),
            lay: Layouts::default(),
            locs: None,
            tracked: HashMap::new(),
            str_objects: HashMap::new(),
            boxing,
            facts: Default::default(),
            iface_impls: None,
            dyn_modes_memo: HashMap::new(),
        }
    }

    /// Queue the user entry and `velt_main`. Everything else (callees, generic instances, glue,
    /// thunks, vtables) is queued on demand by `func`, so unused functions are never lowered.
    pub(super) fn seed_functions(&mut self) {
        // An async main is reached through its poll function (entry.rs), not its constructor.
        if let Some(e) = self.hir.entry.filter(|&e| !self.is_async_fn(e)) {
            self.func(Work::Fn(e, vec![]));
        }
        self.func(Work::Main);
    }

    pub(super) fn finish(self) -> vir::Program {
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
        let func = super::FnLower::build(self, work);
        self.funcs[fid.0 as usize] = Some(func);
        self.building.remove(&fid);
    }

    /// Instance of a user function with concrete type args.
    pub(super) fn func_for(&mut self, def: DefId, targs: Vec<hir::TyId>) -> FuncId {
        self.func(Work::Fn(def, targs))
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
        object[8..16].copy_from_slice(&(s.len() as u64).to_le_bytes());
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
