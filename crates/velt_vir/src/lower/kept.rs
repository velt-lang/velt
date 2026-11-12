//! A lowering kept for `velt dev` (docs/internals/design/dev-warm-front-end.md step 3, "E"): the
//! final pass's context of a cold lowering, into which a reload of an edit of function bodies
//! alone lowers the functions of the edited files again, and only those (with what they newly
//! reach), instead of the whole program.
//!
//! The reload's checked program comes from the kept check of the same cold build
//! (`velt_sema::KeptCheck::reload`): its definitions and types keep their ids, and new ones
//! (an added closure) come after them. A splice is refused, and the caller lowers the whole
//! program, when the program differs in anything but those bodies as far as lowering can tell:
//! another type table, an interface that changed (`velt_sema::interface`) other than of a
//! closure of an edited file, types counted otherwise once the functions are lowered again, or a
//! diagnostic. A refused splice leaves no context: the next version is lowered cold and kept.

use rustc_hash::FxHashSet;
use velt_common::FileId;
use velt_sema::hir::{self, DefId};

use super::program::work_def;
use super::{Cx, Work};
use crate::vir;
use crate::LowerOptions;

/// The final pass's context of a cold lowering and the checked program it lowered, which it
/// borrows.
pub struct KeptLowering {
    // Dropped before `hir`, which it borrows.
    cx: Option<Box<Cx<'static>>>,
    hir: Option<Box<hir::Program>>,
    /// The counted types the lowering ended with: a splice keeps them.
    seed: super::CountedSeed,
}

impl Drop for KeptLowering {
    fn drop(&mut self) {
        self.cx = None;
    }
}

// SAFETY: the context and the program it borrows move together, and nothing outside them
// holds what they share (the `Rc`s of the context are its own; the programs `finish_copy`
// makes share nothing with it): a kept lowering is used on one thread at a time.
unsafe impl Send for KeptLowering {}

/// What a splice changed in the program the kept lowering last gave: the functions it lowered
/// (again or new), by index, and the program's tables as they are now.
pub struct Delta {
    pub funcs: Vec<(u32, vir::Function)>,
    /// The program's function count.
    pub len: usize,
    pub aggs: Vec<vir::AggLayout>,
    pub externs: Vec<vir::ExternFn>,
    pub statics: Vec<vir::StaticData>,
    pub files: Vec<String>,
    pub debug_types: Vec<vir::DebugTy>,
}

impl Delta {
    /// Make `program` (the last version, its functions possibly optimized since) this version:
    /// the indices of the functions it changed.
    pub fn apply(self, program: &mut vir::Program) -> Vec<usize> {
        let placeholder = |i: usize| {
            vir::Function::new(
                format!("#{i}"),
                vec![],
                vir::Ty::Unit,
                vir::Linkage::Internal,
            )
        };
        while program.funcs.len() < self.len {
            let i = program.funcs.len();
            program.funcs.push(placeholder(i));
        }
        let changed: Vec<usize> = self.funcs.iter().map(|(i, _)| *i as usize).collect();
        for (i, f) in self.funcs {
            program.funcs[i as usize] = f;
        }
        super::keys::disambiguate(&mut program.funcs);
        program.aggs = self.aggs;
        program.externs = self.externs;
        program.statics = self.statics;
        program.files = self.files;
        program.debug_types = self.debug_types;
        changed
    }
}

/// A refused splice: why, and the program handed to it.
pub type Refused = (String, Box<hir::Program>);

impl KeptLowering {
    /// Lower `program` as [`crate::lower_seeded`] does, keeping the final pass's context.
    pub fn lower(
        program: Box<hir::Program>,
        opts: &LowerOptions,
    ) -> Result<(vir::Program, super::CountedSeed, KeptLowering), Vec<velt_common::Diagnostic>>
    {
        // SAFETY: the context lives in the `KeptLowering` that owns `program`, and is dropped
        // first; the box keeps the program's address.
        let hir: &'static hir::Program = unsafe { &*(program.as_ref() as *const hir::Program) };
        let (mut p, diags, seed, cx) = super::lower_program_kept(hir, opts, true, true);
        if !diags.is_empty() {
            return Err(diags);
        }
        // The kept context hands its bodies to the program (`Cx::take_bodies`).
        let mut cx = cx.map(Box::new);
        if let Some(cx) = cx.as_mut() {
            let n = cx.funcs.len();
            for (i, f) in cx.take_bodies(0..n) {
                p.funcs[i as usize].locals = f.locals;
                p.funcs[i as usize].blocks = f.blocks;
                p.funcs[i as usize].locs = f.locs;
            }
        }
        let seed = seed.unwrap_or_default();
        let kept = KeptLowering {
            cx,
            hir: Some(program),
            seed: seed.clone(),
        };
        Ok((p, seed, kept))
    }

    /// The counted types of the lowering, which a splice keeps (`crate::lower_seeded`'s seed).
    pub fn seed(&self) -> &super::CountedSeed {
        &self.seed
    }

    /// The checked program lowered last, the context dropped.
    pub fn into_hir(mut self) -> Box<hir::Program> {
        self.cx = None;
        self.hir
            .take()
            .unwrap_or_else(|| super::ice("a kept lowering without its program"))
    }

    /// The checked program lowered last.
    pub fn hir(&self) -> &hir::Program {
        self.hir
            .as_deref()
            .unwrap_or_else(|| super::ice("a kept lowering without its program"))
    }

    /// Lower `program`, a reload whose edits are in function bodies of `files` alone, into the
    /// kept context: the functions defined in `files` again (a body edit moves the locations of
    /// the functions after it), with what they newly reach. Refused, nothing is kept and the
    /// program comes back, to be lowered whole.
    pub fn splice(
        mut self,
        program: Box<hir::Program>,
        files: &[FileId],
        opts: &LowerOptions,
    ) -> Result<(Delta, KeptLowering), Refused> {
        if let Some(why) = refuse(self.hir(), &program, files) {
            return Err((why, program));
        }
        let Some(cx) = self.cx.as_mut() else {
            return Err(("no lowering kept".into(), program));
        };
        // SAFETY: as in `lower`; the old program is dropped only once the context no longer
        // borrows it (`std_fns` is the only other borrow, cleared here).
        cx.hir = unsafe { &*(program.as_ref() as *const hir::Program) };
        cx.std_fns = std::cell::OnceCell::new();
        drop(self.hir.replace(program));
        cx.locs = (opts.source_map).map(|sm| super::srcloc::LocMap::new(sm, opts.std_root));
        match cx.relower(files) {
            Ok(built) => {
                let delta = cx.delta(built.into_iter());
                Ok((delta, self))
            }
            Err(why) => {
                self.cx = None;
                let program = self.hir.take().unwrap_or_else(|| super::ice("no program"));
                Err((why, program))
            }
        }
    }
}

/// Why `new` cannot be spliced into the lowering of `old`, if it cannot.
fn refuse(old: &hir::Program, new: &hir::Program, files: &[FileId]) -> Option<String> {
    let same_types = old.types.len() == new.types.len()
        && (0..old.types.len() as u32)
            .all(|i| old.types.kind(hir::TyId(i)) == new.types.kind(hir::TyId(i)));
    if !same_types {
        return Some("the edit makes other types".into());
    }
    if new.defs.len() < old.defs.len() {
        return Some("the edit has fewer definitions".into());
    }
    // A closure of a function in an edited file is lowered again with it, whatever its
    // interface; any other interface is everyone's.
    let own: FxHashSet<&str> = (new.defs.iter())
        .filter_map(|d| match d {
            hir::Def::Fn(f) if files.contains(&f.span.file) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    let closure_of_own =
        |name: &str| (name.find("::{closure#")).is_some_and(|at| own.contains(&name[..at]));
    use velt_sema::interface::{Change, Summary};
    let changes = Summary::of(old).changes(&Summary::of(new));
    let other = changes.iter().find(|c| match c {
        Change::Fn(name) => !closure_of_own(name),
        Change::Fact(_) => true,
    });
    other.map(|c| format!("the edit changes the interface of {c}"))
}

impl Cx<'_> {
    /// Lower the functions defined in `files` again, then what they reach that is not lowered
    /// yet, and the functions built from everything else (`velt_main`, the function tables).
    fn relower(&mut self, files: &[FileId]) -> Result<Vec<usize>, String> {
        let edited = |d: DefId| match self.hir.def(d) {
            hir::Def::Fn(f) => files.contains(&f.span.file),
            _ => false,
        };
        // Built from everything else: the function tables and the function releasing the
        // static atomics. (`velt_main` is not built again: what it depends on, whether a task
        // is pinned, is checked to stay as it was.)
        let last = |w: &Work| {
            matches!(
                w,
                Work::StaticsDrop | Work::Glue(super::Glue::FnLength | super::Glue::FnName, _)
            )
        };
        let mut redo: Vec<(vir::FuncId, Work)> = (self.work_map.iter())
            .filter(|(w, _)| work_def(w).is_some_and(edited) || matches!(w, Work::ModuleInit(..)))
            .map(|(w, &id)| (id, w.clone()))
            .collect();
        let mut after: Vec<(vir::FuncId, Work)> = (self.work_map.iter())
            .filter(|(w, _)| last(w))
            .map(|(w, &id)| (id, w.clone()))
            .collect();
        redo.sort_by_key(|(id, _)| id.0);
        after.sort_by_key(|(id, _)| id.0);
        for (id, _) in redo.iter().chain(&after) {
            self.funcs[id.0 as usize] = None;
            self.building.remove(id);
        }
        let placement = self.uses_placement;
        let counted = self.boxing.clone();
        let facts = self.facts.clone();
        self.built_log = Some(vec![]);
        for (id, work) in &redo {
            self.build_now(*id, work);
        }
        self.drain();
        self.build_iface_glues();
        for (id, work) in &after {
            self.build_now(*id, work);
        }
        self.drain();
        self.build_iface_glues();
        if self.uses_placement != placement {
            return Err("the edit pins a task".into());
        }
        self.build_fn_tables();
        // What lowering learns of the program as it goes (`boxing::Facts`) only grows: the same
        // facts after the edited functions are lowered again mean no other function would be
        // lowered otherwise (its counting, its parameter attributes, the glue it gets).
        if self.facts != facts {
            return Err("the edit tells lowering something new of the program".into());
        }
        let next = self.close_boxing();
        if next != counted {
            return Err(format!(
                "the edit counts other types: {}",
                counted.changes(&next)
            ));
        }
        self.unfreeze_module_reach();
        self.version_deferred();
        let diags = [
            self.nothing_clash_diagnostics(),
            self.fresh_fn_diagnostics(),
            self.counted_atomic_diagnostics(),
            self.counted_lock_diagnostics(),
            self.atomic_copy_diagnostics(),
            self.unavailable_diagnostics(),
            self.unreduced_diagnostics(),
        ];
        if let Some(d) = diags.iter().flatten().next() {
            return Err(format!("lowering reports: {}", d.message));
        }
        if self.funcs.iter().any(Option::is_none) {
            return Err("a function was left unlowered".into());
        }
        let built = self.built_log.take().unwrap_or_default();
        Ok(built.into_iter().map(|f| f.0 as usize).collect())
    }

    /// The functions `which` as a [`Delta`], with the program's tables.
    fn delta(&mut self, which: impl Iterator<Item = usize>) -> Delta {
        self.fill_registry();
        let mut which: Vec<usize> = which.collect();
        which.sort_unstable();
        which.dedup();
        Delta {
            funcs: self.take_bodies(which.into_iter()),
            len: self.funcs.len(),
            aggs: self.aggs.clone(),
            externs: self.externs.clone(),
            statics: self.statics.clone(),
            files: self
                .locs
                .as_ref()
                .map(|m| m.files.clone())
                .unwrap_or_default(),
            debug_types: self
                .debug
                .as_ref()
                .map(|d| d.types.clone())
                .unwrap_or_default(),
        }
    }

    /// The bodies of the functions `which`, each leaving behind a copy without statements,
    /// locals and locations: what lowering reads of a function it built (its signature,
    /// attributes and calls) stays.
    fn take_bodies(&mut self, which: impl Iterator<Item = usize>) -> Vec<(u32, vir::Function)> {
        let mut out = vec![];
        for i in which {
            let Some(full) = self.funcs.get_mut(i).and_then(Option::as_mut) else {
                continue;
            };
            let locals = std::mem::take(&mut full.locals);
            let locs = std::mem::take(&mut full.locs);
            let stmts: Vec<Vec<vir::Stmt>> = (full.blocks.iter_mut())
                .map(|b| std::mem::take(&mut b.stmts))
                .collect();
            let mut body = full.clone();
            body.locals = locals;
            body.locs = locs;
            for (b, s) in body.blocks.iter_mut().zip(stmts) {
                b.stmts = s;
            }
            out.push((i as u32, body));
        }
        out
    }

    /// Build what is queued, the function releasing the static atomics last.
    fn drain(&mut self) {
        while let Some((fid, work)) = self.queue.pop_front() {
            if matches!(work, Work::StaticsDrop) && !self.queue.is_empty() {
                self.queue.push_back((fid, work));
                continue;
            }
            self.build_now(fid, &work);
        }
    }
}
