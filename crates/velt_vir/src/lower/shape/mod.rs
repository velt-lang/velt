//! Generic instances shared by shape (docs/internals/design/shape-sharing.md), step 2: debug
//! builds, the array-view sync helpers.
//!
//! A call of a shareable generic function from code of concrete types calls one instance per
//! *shape* of its type arguments (key.rs), passing a dictionary of the type-specific code as a
//! hidden parameter after the user's arguments. The shared instance is lowered with a
//! *placeholder* for each shared type argument: a `TyKind::Param` at `SHAPE_BASE` or above,
//! which HIR never holds.
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
//! Shared bodies are lowered in every pass. The facts a shared body records over placeholders
//! are taken out of the pass's facts once it is lowered and added back for each of its
//! dictionaries with that dictionary's types (`finish_shared`, `replay`; boxing/replay.rs), so
//! the passes find the counted types they find without sharing. Facts kept per function
//! (boxing/tuples.rs) can't be replayed, since one shared `FuncId` stands for several
//! instances: a shared body recording one over a placeholder is an ICE.

mod dict;
mod glue;
mod key;
mod rewrite;

use rustc_hash::FxHashMap;
use velt_sema::hir::{DefId, TyId, TyKind};

use super::{ice, Cx, LkMode, Work};
use crate::vir::{FuncId, StaticId};

pub(super) use key::Key;

/// The functions shared `Map` code calls for its key type's hash and comparison (intrinsics.rs
/// `ph_key_call`): never shared themselves, since their bodies are the key type's own code.
pub(super) const KEY_HASH: &str = "std/lang/map/key::keyHash";
pub(super) const KEY_SAME: &str = "std/lang/map/key::keySame";

/// Functions of the `Map` family kept per type: the key functions themselves, and those writing a
/// key's text (`${key}`), which needs the type's format code.
const MAP_PER_TYPE: &[&str] = &[KEY_HASH, KEY_SAME, "::mapKeyNames", "mapNewestKeyDigit"];

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
    // What they call at their element type: dropping the mirror's tail.
    "std/prelude/array_edit::T[].truncate",
];

/// One placeholder: its type, the shape's representative and every concrete type of the
/// shape at that position.
struct Ph {
    ty: TyId,
    /// Its name in symbols and debug types (`ph_name`).
    name: String,
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
    /// Each dictionary made for it, with its concrete type arguments and the user code that
    /// asked for it (the site its replayed shares report).
    dicts: Vec<(StaticId, Vec<TyId>, velt_common::Span)>,
    /// The facts its body recorded over placeholders in this pass, replayed per dictionary.
    facts: Option<super::boxing::Facts>,
    /// The dictionary parameter, once declared (abi.rs).
    pub(super) param: Option<crate::vir::Local>,
    /// How many arguments follow the dictionary: the caller's location of a caller-tracking
    /// instance, and the out-pointer of an aggregate return.
    trailing: usize,
}

/// The shape-sharing state of one pass.
#[derive(Default)]
pub(super) struct Shapes {
    /// Sharing is on in this pass.
    pub(super) on: bool,
    /// Sharing is wanted (debug builds, `VELT_SHAPES`).
    pub(super) wanted: bool,
    /// A `ToAny` / `FromAny` glue is being built: its code is what the shared std body would
    /// have written at its site, std code for what lowering notes (`note_array_view`).
    pub(super) std_glue: bool,
    ph: Vec<Ph>,
    ph_of: FxHashMap<(Key, u32), TyId>,
    /// Memo of `placeholder`: a type's key is fixed for the pass (`Boxing` is), so each call
    /// of a shared helper doesn't compute it again.
    ph_for: FxHashMap<(TyId, u32), Option<TyId>>,
    pub(super) insts: FxHashMap<FuncId, Inst>,
    pseudo: Vec<(FuncId, u32)>,
    /// The shared instances being lowered, innermost last.
    pub(super) current: Vec<FuncId>,
    dicts: FxHashMap<(FuncId, Vec<TyId>), StaticId>,
    /// Memo of `mentions_ph`, by type index: 0 not known yet, 1 no, 2 yes.
    mentions: std::cell::RefCell<Vec<u8>>,
    /// The types built from placeholders that shape questions were asked about (key.rs).
    asked: std::cell::RefCell<rustc_hash::FxHashSet<TyId>>,
}

impl Shapes {
    /// No placeholder exists in this pass (no shared instance): every shape question is the
    /// question itself.
    #[inline]
    pub(super) fn no_placeholders(&self) -> bool {
        self.ph.is_empty()
    }

    /// Shared instances made in this pass, and the per-type instances they stand for.
    pub(super) fn counts(&self) -> (usize, usize) {
        let per_type = self.insts.values().map(|i| i.dicts.len()).sum();
        (self.insts.len(), per_type)
    }
}

fn maps_wanted() -> bool {
    std::env::var_os("VELT_SHAPES_MAPS").is_some()
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
    /// `t`'s kind as the type table has it, for code that matches on it directly. A
    /// placeholder has none (an ICE): only the shape questions answer for it (`ph_rep`).
    #[inline]
    pub(super) fn raw_kind(&self, t: TyId) -> &TyKind {
        if self.is_ph(t) {
            ice("a placeholder's kind read directly");
        }
        self.types.kind(t)
    }

    /// Is `t` a placeholder type?
    #[inline]
    pub(super) fn is_ph(&self, t: TyId) -> bool {
        !self.shapes.no_placeholders()
            && matches!(self.types.kind(t), TyKind::Param(n) if *n >= SHAPE_BASE)
    }

    /// Per type of the table, whether it mentions a placeholder of an earlier pass (the type
    /// table outlives a pass, its placeholders don't), which this pass cannot name. One sweep:
    /// a type's parts are interned before it, so their answers are known (a later part is
    /// walked).
    pub(super) fn stale_ph_types(&self) -> Vec<bool> {
        let live = self.shapes.ph.len() as u32;
        let stale_kind = |k: &TyKind| matches!(k, TyKind::Param(n) if *n >= SHAPE_BASE && *n - SHAPE_BASE >= live);
        let mut out: Vec<bool> = Vec::with_capacity(self.types.len());
        for i in 0..self.types.len() {
            let k = self.types.kind(TyId(i as u32));
            let stale = stale_kind(k)
                || parts(k).into_iter().any(|p| match out.get(p.0 as usize) {
                    Some(&known) => known,
                    None => {
                        let mut stack = vec![p];
                        let mut seen = rustc_hash::FxHashSet::default();
                        let mut found = false;
                        while let Some(x) = stack.pop() {
                            if !seen.insert(x) {
                                continue;
                            }
                            let k = self.types.kind(x);
                            if stale_kind(k) {
                                found = true;
                                break;
                            }
                            stack.extend(parts(k));
                        }
                        found
                    }
                });
            out.push(stale);
        }
        out
    }

    /// Does `t` mention a placeholder?
    #[inline]
    pub(super) fn mentions_ph(&self, t: TyId) -> bool {
        !self.shapes.no_placeholders() && mentions(&self.shapes, &self.types, t)
    }

    /// The placeholder for concrete type `t` at type-argument position `pos`, made when new;
    /// `None` when `t` is not shared (key.rs).
    fn placeholder(&mut self, t: TyId, pos: u32) -> Option<TyId> {
        if let Some(&p) = self.shapes.ph_for.get(&(t, pos)) {
            return p;
        }
        let p = self.new_placeholder(t, pos);
        self.shapes.ph_for.insert((t, pos), p);
        p
    }

    /// [`placeholder`](Self::placeholder) without the memo.
    fn new_placeholder(&mut self, t: TyId, pos: u32) -> Option<TyId> {
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
        let name = key.name();
        self.shapes.ph.push(Ph {
            name,
            ty: p,
            rep: t,
            members: vec![t],
        });
        self.shapes.ph_of.insert((key, pos), p);
        Some(p)
    }

    /// The name of placeholder `p`: its layout and a hash of the rest of its key, stable
    /// across edits that keep its shape (symbols name it; `velt dev` matches by symbol).
    pub(super) fn ph_name(&self, p: TyId) -> String {
        self.shapes.ph[self.ph_index(p)].name.clone()
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
        self.shared_call_at(def, targs, mode, None)
    }

    /// [`shared_call`](Self::shared_call) of a caller-tracking function that takes its
    /// caller's location as a parameter (`at`: `srcloc::ARG_LOC`, track_caller.rs).
    pub(super) fn shared_call_at(
        &mut self,
        def: DefId,
        targs: &[TyId],
        mode: LkMode,
        at: Option<crate::vir::SrcLoc>,
    ) -> Option<(FuncId, StaticId)> {
        if !self.shapes.on
            || targs.is_empty()
            || !self.shares(def)
            || targs.iter().any(|&t| self.mentions_ph(t))
        {
            return None;
        }
        let mut phs = Vec::with_capacity(targs.len());
        for (i, &t) in targs.iter().enumerate() {
            phs.push(self.placeholder(t, i as u32)?);
        }
        let work = match at {
            Some(at) => Work::Tracked(def, phs, at),
            None => Work::Fn(def, phs),
        };
        let f = self.func_direct(work, mode);
        let dict = self.dict_for(f, targs.to_vec());
        Some((f, dict))
    }

    /// Is `f` a shared instance?
    pub(super) fn is_shared_inst(&self, f: FuncId) -> bool {
        self.shapes.insts.contains_key(&f)
    }

    /// Is `def` one of the functions shared in this step?
    fn shares(&self, def: DefId) -> bool {
        match self.hir.def(def) {
            velt_sema::hir::Def::Fn(f) => {
                SHARED.contains(&f.name.as_str())
                    || (maps_wanted()
                        && !MAP_PER_TYPE.iter().any(|n| f.name.contains(n))
                        && (f.name.starts_with("std/lang/map/")
                            || f.name.starts_with("std/prelude/map::Map.")))
            }
            _ => false,
        }
    }

    /// `func_in`'s view of `work` in a pass that shares: `Some(id)` for a pseudo id (work at a
    /// placeholder type inside a shared instance), `None` to build `work` as usual. A shared
    /// instance itself is registered (`register_inst`) once it has its id.
    #[inline]
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
        let (d, targs) = match inner {
            Work::Fn(d, targs) => (d, targs),
            // A caller-tracking instance: its site is the same for every shape.
            Work::Tracked(d, targs, _) => (d, targs),
            _ => return None,
        };
        let canonical = targs.iter().enumerate().all(|(i, &t)| {
            self.is_ph(t)
                && self
                    .shapes
                    .ph_of
                    .iter()
                    .any(|(k, &p)| p == t && k.1 == i as u32)
        });
        (!targs.is_empty() && self.shares(*d) && canonical).then(|| (*d, targs.clone()))
    }

    /// Whether `work` is a shared instance (`register_inst` then records it).
    pub(super) fn work_names_shared(&self, work: &Work) -> bool {
        self.shared_inst_key(work).is_some()
    }

    /// Record shared instance `id` for `work` (after `func_in` made its id).
    pub(super) fn register_inst(&mut self, id: FuncId, work: &Work) {
        if self.shapes.insts.contains_key(&id) {
            return;
        }
        if let Some((def, targs)) = self.shared_inst_key(work) {
            // Only an instance taking its caller's location has the parameter (track_caller.rs).
            let arg_loc =
                |w: &Work| matches!(w, Work::Tracked(_, _, at) if *at == super::srcloc::ARG_LOC);
            let tracked = arg_loc(work)
                || matches!(work, Work::Checked(w) | Work::Read(w) | Work::ReadOwn(w)
                    | Work::Deferred(w) if arg_loc(w));
            let f = self.fn_def(def);
            let throws = self.fn_throws(f, &targs);
            let ret = self.subst(f.ret, &targs);
            let out = self.ret_abi(ret, throws).out.is_some();
            let inst = Inst {
                trailing: usize::from(tracked) + usize::from(out),
                def,
                targs,
                entries: vec![],
                index: FxHashMap::default(),
                dicts: vec![],
                facts: None,
                param: None,
            };
            self.shapes.insts.insert(id, inst);
        }
    }

    /// Before shared instance `fid` is lowered: it is the current one. `false` for any other
    /// function.
    pub(super) fn start_shared(&mut self, fid: FuncId) -> bool {
        if !self.shapes.insts.contains_key(&fid) {
            return false;
        }
        self.shapes.current.push(fid);
        true
    }

    /// After a function is lowered: a shared instance gets its pseudo ids rewritten
    /// (rewrite.rs), and the facts its body recorded over placeholders are taken out of the
    /// pass's and replayed for each of its dictionaries (`replay`; boxing/replay.rs).
    pub(super) fn finish_shared(
        &mut self,
        fid: FuncId,
        mut func: crate::vir::Function,
        shared: bool,
        deferred_mark: usize,
    ) -> crate::vir::Function {
        if !shared {
            return func;
        }
        self.shapes.current.pop();
        // Facts kept per function (boxing/tuples.rs) are not replayed: one shared `FuncId`
        // stands for several instances. The body's own are still pending here (`finish_fn_tuples`
        // stores them after this), so they are checked here.
        // Test hook (`VELT_SHAPES_TEST_DROP=per-function`): the body records one, so a test
        // shows that this check fires.
        if key::dropped("per-function") {
            if let Some(&p) = self.shapes.insts[&fid].targs.first() {
                self.cur_read_params.push(p);
            }
        }
        let own = self.cur_tuple_writes.iter().chain(&self.cur_read_params);
        if own.copied().any(|t| self.mentions_ph(t)) {
            ice("a shared body's own facts per function are not replayed");
        }
        // A body that left its view loops for later stands as its calls until they are versioned
        // (deferred_versions.rs), and is rewritten then.
        if self.deferred_new.len() > deferred_mark {
            self.rewrite_stand_in(fid, &mut func);
        } else {
            self.shape_rewrite(fid, &mut func);
        }
        let facts = {
            let shapes = &self.shapes;
            let types = &self.types;
            let ph = |t: TyId| mentions(shapes, types, t);
            self.facts.take_types(&ph)
        };
        if facts.by_function() {
            ice("a fact kept per function names a placeholder, which is not replayed");
        }
        self.shapes.insts.get_mut(&fid).expect("ICE: shared").facts = Some(facts);
        let dicts: Vec<(Vec<TyId>, velt_common::Span)> = (self.shapes.insts[&fid].dicts.iter())
            .map(|(_, t, s)| (t.clone(), *s))
            .collect();
        for (targs, site) in dicts {
            self.replay(fid, &targs, site);
        }
        func
    }

    /// The facts shared instance `inst`'s body recorded, for its dictionary of `targs`, added
    /// to the pass's.
    pub(super) fn replay(&mut self, inst: FuncId, targs: &[TyId], site: velt_common::Span) {
        let Some(facts) = self
            .shapes
            .insts
            .get_mut(&inst)
            .and_then(|i| i.facts.take())
        else {
            return;
        };
        let ph: Vec<(TyId, TyId)> = (self.shapes.insts[&inst].targs.iter().copied())
            .zip(targs.iter().copied())
            .collect();
        let mut map = FxHashMap::default();
        for t in super::boxing::Facts::types_of(&facts) {
            if let std::collections::hash_map::Entry::Vacant(e) = map.entry(t) {
                e.insert(self.replace_tys(t, &ph));
            }
        }
        let shared: Vec<TyId> = (facts.shared_types().iter())
            .filter_map(|t| map.get(t).copied())
            .collect();
        self.facts.add_mapped(&facts, &map);
        if site != velt_common::Span::DUMMY {
            let outer = std::mem::replace(&mut self.user_site, site);
            for t in shared {
                self.note_share_site(t);
            }
            self.user_site = outer;
        }
        self.shapes.insts.get_mut(&inst).expect("ICE: shared").facts = Some(facts);
    }

    /// `args` of a call of shared instance `inst` with its dictionary `dict` put in its place,
    /// after the user's arguments (abi.rs `declare_locals`).
    pub(super) fn with_dict(
        &self,
        inst: FuncId,
        args: &mut Vec<crate::vir::Operand>,
        dict: crate::vir::Operand,
    ) {
        let at = args.len() - self.shapes.insts[&inst].trailing;
        args.insert(at, dict);
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

/// Does `t` mention a placeholder (`Cx::mentions_ph`, for borrows that keep `Cx` apart)?
fn mentions(shapes: &Shapes, types: &velt_sema::hir::TyTable, t: TyId) -> bool {
    if shapes.ph.is_empty() {
        return false;
    }
    match shapes.mentions.borrow().get(t.0 as usize) {
        Some(1) => return false,
        Some(2) => return true,
        _ => {}
    }
    let mut stack = vec![t];
    let mut found = false;
    let mut seen = rustc_hash::FxHashSet::default();
    while let Some(x) = stack.pop() {
        if !seen.insert(x) {
            continue;
        }
        match types.kind(x) {
            TyKind::Param(n) if *n >= SHAPE_BASE => {
                found = true;
                break;
            }
            k => stack.extend(parts(k)),
        }
    }
    let mut memo = shapes.mentions.borrow_mut();
    if memo.len() <= t.0 as usize {
        memo.resize(types.len().max(t.0 as usize + 1), 0);
    }
    memo[t.0 as usize] = if found { 2 } else { 1 };
    found
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
