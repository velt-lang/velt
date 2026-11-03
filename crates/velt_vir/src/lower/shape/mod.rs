//! Generic instances shared by shape (docs/internals/design/shape-sharing.md), step 2: debug
//! builds, the array-view sync helpers.
//!
//! A call of a shareable generic function from code of concrete types calls one instance per
//! *shape* of its type arguments (key.rs), passing a dictionary of the type-specific code as a
//! hidden last parameter. The shared instance is lowered with a *placeholder* for each shared
//! type argument: a `TyKind::Param` at `SHAPE_BASE` or above, which HIR never holds.
//! - Layout and ownership questions (the shape questions) about a placeholder, or a type built
//!   from one, are answered for the same type built from its representative, the first
//!   concrete type of the shape (`Cx::ph_rep`). Every type asked about this way is checked
//!   against every member of the shape at the end of the pass (`shapes_close`): a key too
//!   coarse is an ICE, never wrong code.
//! - Any other question about a placeholder (`Cx::kind`) is an ICE.
//! - A function the shared body needs at a placeholder type (glue, a generic callee) gets a
//!   pseudo id (`PSEUDO_BASE` and up) instead of being built; after the body is lowered, the
//!   pseudo ids become loads of dictionary entries and indirect calls (rewrite.rs), and each
//!   dictionary holds the concrete function for its type arguments (dict.rs). A call of
//!   another shared instance passes that instance's dictionary, an entry of this one.
//!
//! Sharing happens only in the pass that lowers the program whole, once the counted types are
//! complete: the facts a shared body would record are dropped (a facts-only pass lowers the
//! per-type instances and records them), so the passes converge exactly as without sharing.

mod dict;
mod glue;
mod key;
mod rewrite;

use rustc_hash::FxHashMap;
use velt_sema::hir::{DefId, TyId, TyKind};

use super::{ice, Cx, LkMode, Work};
use crate::vir::{FuncId, StaticId};

pub(super) use key::Key;

/// Placeholder types are `TyKind::Param(SHAPE_BASE + i)`.
pub(super) const SHAPE_BASE: u32 = 1 << 30;
/// Pseudo function ids: entry `i` of `Shapes::pseudo` is `FuncId(PSEUDO_BASE + i)`.
const PSEUDO_BASE: u32 = 0xC000_0000;

/// The prelude functions shared in this step: the array-view sync helpers (`arrayViewSort`
/// takes a closure and is not).
const SHARED: &[&str] = &[
    velt_common::std_hook!(array_view_sync),
    velt_common::std_hook!(array_view_sync_at),
    velt_common::std_hook!(array_view_sync_len),
    velt_common::std_hook!(array_view_flush),
    velt_common::std_hook!(array_view_flush_from),
    velt_common::std_hook!(array_view_flush_at),
    velt_common::std_hook!(array_view_len),
];

/// One placeholder: its type, the shape's representative and every concrete type of the
/// shape at that position.
struct Ph {
    ty: TyId,
    rep: TyId,
    members: Vec<TyId>,
}

/// What a dictionary entry holds for each concrete instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Entry {
    /// The address of the function for `Work` (placeholders substituted) in that mode.
    Code(Work, LkMode),
    /// The dictionary of shared instance `FuncId` at the substituted type arguments.
    Dict(FuncId),
}

/// A shared instance.
pub(super) struct Inst {
    def: DefId,
    /// Its type arguments: placeholders.
    targs: Vec<TyId>,
    entries: Vec<Entry>,
    index: FxHashMap<Entry, u32>,
    /// Each dictionary made for it, with its concrete type arguments.
    dicts: Vec<(StaticId, Vec<TyId>)>,
    /// The dictionary parameter, once declared (abi.rs).
    pub(super) param: Option<crate::vir::Local>,
}

/// The shape-sharing state of one pass.
#[derive(Default)]
pub(super) struct Shapes {
    /// Sharing is on in this pass.
    pub(super) on: bool,
    /// Sharing is on in the pass that lowers the program whole.
    pub(super) wanted: bool,
    ph: Vec<Ph>,
    ph_of: FxHashMap<(Key, u32), TyId>,
    pub(super) insts: FxHashMap<FuncId, Inst>,
    pseudo: Vec<(FuncId, u32)>,
    /// The shared instances being lowered, innermost last.
    pub(super) current: Vec<FuncId>,
    dicts: FxHashMap<(FuncId, Vec<TyId>), StaticId>,
    /// Memo of `mentions_ph`.
    mentions: std::cell::RefCell<FxHashMap<TyId, bool>>,
    /// The types built from placeholders that shape questions were asked about (key.rs).
    asked: std::cell::RefCell<rustc_hash::FxHashSet<TyId>>,
}

impl Shapes {
    /// Shared instances made in this pass, and the per-type instances they stand for.
    pub(super) fn counts(&self) -> (usize, usize) {
        let per_type = self.insts.values().map(|i| i.dicts.len()).sum();
        (self.insts.len(), per_type)
    }
}

/// Whether `VELT_SHAPES` turns sharing off (`off`) or on in every build (`all`); unset, the
/// driver's choice (debug builds) stands.
pub(super) fn wanted(debug_build: bool) -> bool {
    match std::env::var("VELT_SHAPES").as_deref() {
        Ok("off") => false,
        Ok("all") => true,
        _ => debug_build,
    }
}

impl Cx<'_> {
    /// Is `t` a placeholder type?
    pub(super) fn is_ph(&self, t: TyId) -> bool {
        matches!(self.types.kind(t), TyKind::Param(n) if *n >= SHAPE_BASE)
    }

    /// Does `t` mention a placeholder?
    pub(super) fn mentions_ph(&self, t: TyId) -> bool {
        if self.shapes.ph.is_empty() {
            return false;
        }
        if let Some(&m) = self.shapes.mentions.borrow().get(&t) {
            return m;
        }
        let mut stack = vec![t];
        let mut found = false;
        let mut seen = rustc_hash::FxHashSet::default();
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            match self.types.kind(x) {
                TyKind::Param(n) if *n >= SHAPE_BASE => {
                    found = true;
                    break;
                }
                k => stack.extend(parts(k)),
            }
        }
        self.shapes.mentions.borrow_mut().insert(t, found);
        found
    }

    /// The placeholder for concrete type `t` at type-argument position `pos`, made when new;
    /// `None` when `t` is not shared (key.rs).
    fn placeholder(&mut self, t: TyId, pos: u32) -> Option<TyId> {
        let key = self.shape_key(t)?;
        if let Some(&p) = self.shapes.ph_of.get(&(key.clone(), pos)) {
            let i = self.ph_index(p);
            if !self.shapes.ph[i].members.contains(&t) {
                self.shapes.ph[i].members.push(t);
            }
            return Some(p);
        }
        let n = SHAPE_BASE + self.shapes.ph.len() as u32;
        let p = self.intern(TyKind::Param(n));
        self.shapes.ph.push(Ph {
            ty: p,
            rep: t,
            members: vec![t],
        });
        self.shapes.ph_of.insert((key, pos), p);
        Some(p)
    }

    fn ph_index(&self, p: TyId) -> usize {
        match self.types.kind(p) {
            TyKind::Param(n) if *n >= SHAPE_BASE => (*n - SHAPE_BASE) as usize,
            _ => ice("not a placeholder type"),
        }
    }

    /// The shared instance and dictionary for a call of `def` at concrete `targs` from code of
    /// mode `mode`, when `def` is shared in this pass.
    pub(super) fn shared_call(
        &mut self,
        def: DefId,
        targs: &[TyId],
        mode: LkMode,
    ) -> Option<(FuncId, StaticId)> {
        if !self.shares(def) || targs.iter().any(|&t| self.mentions_ph(t)) {
            return None;
        }
        if !self.shapes.on {
            // A facts-only pass: the program is lowered whole once more, sharing.
            self.left_out |= self.shapes.wanted && self.facts_only;
            return None;
        }
        let mut phs = Vec::with_capacity(targs.len());
        for (i, &t) in targs.iter().enumerate() {
            phs.push(self.placeholder(t, i as u32)?);
        }
        let f = self.func_direct(Work::Fn(def, phs), mode);
        let dict = self.dict_for(f, targs.to_vec());
        Some((f, dict))
    }

    /// Is `def` one of the functions shared in this step?
    fn shares(&self, def: DefId) -> bool {
        match self.hir.def(def) {
            velt_sema::hir::Def::Fn(f) => SHARED.contains(&f.name.as_str()),
            _ => false,
        }
    }

    /// `func_in`'s view of `work` in a pass that shares: `Some(id)` for a pseudo id (work at a
    /// placeholder type inside a shared instance), `None` to build `work` as usual. A shared
    /// instance itself is registered (`register_inst`) once it has its id.
    pub(super) fn shape_work(&mut self, work: &Work, mode: LkMode) -> Option<FuncId> {
        if self.shapes.ph.is_empty() || !self.work_mentions_ph(work) {
            return None;
        }
        if self.shared_inst_key(work).is_some() {
            return None;
        }
        let Some(&cur) = self.shapes.current.last() else {
            ice(format_args!(
                "a function at a placeholder type outside a shared instance: {work:?}"
            ))
        };
        Some(self.pseudo_entry(cur, Entry::Code(work.clone(), mode)))
    }

    /// `(def, placeholders)` when `work` (in any mode) is a shared instance.
    fn shared_inst_key(&self, work: &Work) -> Option<(DefId, Vec<TyId>)> {
        let inner = match work {
            Work::Checked(w) | Work::Read(w) | Work::ReadOwn(w) | Work::Deferred(w) => w,
            w => w,
        };
        let Work::Fn(d, targs) = inner else {
            return None;
        };
        let canonical = targs.iter().enumerate().all(|(i, &t)| {
            self.is_ph(t) && self.shapes.ph_of.iter().any(|(k, &p)| p == t && k.1 == i as u32)
        });
        (self.shares(*d) && canonical).then(|| (*d, targs.clone()))
    }

    /// Record shared instance `id` for `work` (after `func_in` made its id).
    pub(super) fn register_inst(&mut self, id: FuncId, work: &Work) {
        if self.shapes.insts.contains_key(&id) {
            return;
        }
        if let Some((def, targs)) = self.shared_inst_key(work) {
            let inst = Inst {
                def,
                targs,
                entries: vec![],
                index: FxHashMap::default(),
                dicts: vec![],
                param: None,
            };
            self.shapes.insts.insert(id, inst);
        }
    }

    /// Before shared instance `fid` is lowered: it is the current one, and the facts its body
    /// records are set aside (module docs); `None` for any other function.
    pub(super) fn start_shared(&mut self, fid: FuncId) -> Option<super::boxing::Facts> {
        if !self.shapes.insts.contains_key(&fid) {
            return None;
        }
        self.shapes.current.push(fid);
        Some(std::mem::take(&mut self.facts))
    }

    /// After a function is lowered: a shared instance gets its facts back and its pseudo ids
    /// rewritten (rewrite.rs).
    pub(super) fn finish_shared(
        &mut self,
        fid: FuncId,
        mut func: crate::vir::Function,
        outer: Option<super::boxing::Facts>,
    ) -> crate::vir::Function {
        let Some(facts) = outer else {
            return func;
        };
        self.facts = facts;
        self.shapes.current.pop();
        self.shape_rewrite(fid, &mut func);
        func
    }

    /// Does `work` mention a placeholder type?
    fn work_mentions_ph(&self, work: &Work) -> bool {
        work_tys(work).into_iter().any(|t| self.mentions_ph(t))
    }

    /// Is the function being declared (`def` at `targs`) the shared instance being lowered?
    /// Its dictionary parameter is then `Some` once declared.
    pub(super) fn shared_being_built(&self, def: DefId, targs: &[TyId]) -> Option<FuncId> {
        let &cur = self.shapes.current.last()?;
        let inst = self.shapes.insts.get(&cur)?;
        (inst.def == def && inst.targs == targs && inst.param.is_none()).then_some(cur)
    }
}

/// The types a `Work` names (those that can mention a placeholder).
fn work_tys(work: &Work) -> Vec<TyId> {
    let mut w = work.clone();
    match &mut w {
        Work::Glue(_, t)
        | Work::AllPoll(t)
        | Work::AllDrop(t)
        | Work::RaceBoxPoll(t)
        | Work::RaceBoxDrop(t)
        | Work::AdoptPoll(t)
        | Work::AdoptDrop(t)
        | Work::Unclaimed(t) => vec![*t],
        Work::WidenPoll(a, b) | Work::WidenDrop(a, b) => vec![*a, *b],
        Work::Init(t, e) => std::iter::once(*t).chain(*e).collect(),
        other => super::program::work_target(other).map_or_else(Vec::new, |(_, a)| a.clone()),
    }
}

/// The types a type is built from (one level).
fn parts(k: &TyKind) -> Vec<TyId> {
    match k {
        TyKind::Adt(_, ts) | TyKind::Dyn(_, ts) | TyKind::Tuple(ts) | TyKind::Op(_, ts) => {
            ts.clone()
        }
        TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => vec![*e],
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![*a, *b],
        TyKind::FnPtr {
            params,
            ret,
            throws,
            ..
        } => params.iter().copied().chain([*ret, *throws]).collect(),
        _ => vec![],
    }
}
