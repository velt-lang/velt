//! Dictionaries: one read-only static of function and dictionary addresses per shared
//! instance and list of concrete type arguments (design note section 2). Entries are found
//! while the shared instance is lowered, so the statics grow as glue/generic_slots.rs's tables
//! do: a new entry is appended to every dictionary made so far, and a dictionary made later
//! starts with every entry known by then.

use velt_sema::hir::TyId;

use super::super::{Cx, Work};
use super::{Entry, PSEUDO_BASE};
use crate::vir::{Const, FuncId, StaticData, StaticId};

impl Cx<'_> {
    /// The dictionary of shared instance `inst` for concrete `targs` (built once).
    pub(super) fn dict_for(&mut self, inst: FuncId, targs: Vec<TyId>) -> StaticId {
        if let Some(&s) = self.shapes.dicts.get(&(inst, targs.clone())) {
            return s;
        }
        // Never empty, for the backends: a dictionary of no entries holds one null word.
        self.statics.push(StaticData {
            bytes: vec![0; 8],
            align: 8,
            relocs: vec![],
            mutable: None,
        });
        let id = StaticId(self.statics.len() as u32 - 1);
        self.shapes.dicts.insert((inst, targs.clone()), id);
        let entries = self.shapes.insts[&inst].entries.clone();
        self.shapes
            .insts
            .get_mut(&inst)
            .expect("ICE: registered")
            .dicts
            .push((id, targs.clone()));
        for e in &entries {
            self.push_dict_entry(id, inst, &targs, e);
        }
        id
    }

    /// The index of `entry` in the dictionaries of `inst`, added to each when new, as a
    /// pseudo function id the shared body uses until it is rewritten.
    pub(super) fn pseudo_entry(&mut self, inst: FuncId, entry: Entry) -> FuncId {
        let i = self.entry_index(inst, entry);
        let k = self.shapes.pseudo.len() as u32;
        self.shapes.pseudo.push((inst, i));
        FuncId(PSEUDO_BASE + k)
    }

    /// The index of `entry` in `inst`'s dictionaries, appended to each when new.
    pub(super) fn entry_index(&mut self, inst: FuncId, entry: Entry) -> u32 {
        let it = self
            .shapes
            .insts
            .get_mut(&inst)
            .expect("ICE: shared instance");
        if let Some(&i) = it.index.get(&entry) {
            return i;
        }
        let i = it.entries.len() as u32;
        it.entries.push(entry.clone());
        it.index.insert(entry.clone(), i);
        let dicts = it.dicts.clone();
        for (id, targs) in dicts {
            self.push_dict_entry(id, inst, &targs, &entry);
        }
        i
    }

    /// The (dictionary entry index) a pseudo id stands for, if it is one.
    pub(super) fn pseudo_of(&self, f: FuncId) -> Option<(FuncId, u32)> {
        f.0.checked_sub(PSEUDO_BASE)
            .and_then(|k| self.shapes.pseudo.get(k as usize).copied())
    }

    /// Appends `entry`, for dictionary `id` of `inst` at `targs`, to the static.
    fn push_dict_entry(&mut self, id: StaticId, inst: FuncId, targs: &[TyId], entry: &Entry) {
        let map: Vec<(TyId, TyId)> = (self.shapes.insts[&inst].targs.iter().copied())
            .zip(targs.iter().copied())
            .collect();
        let c = match entry {
            Entry::Code(work, mode) => {
                let w = self.subst_work(work, &map);
                Const::Func(self.func_in_mode(w, *mode))
            }
            Entry::Dict(callee) => {
                let ctargs: Vec<TyId> = self.shapes.insts[callee].targs.clone();
                let ctargs = ctargs.iter().map(|&t| self.replace_tys(t, &map)).collect();
                Const::Static(self.dict_for(*callee, ctargs))
            }
        };
        let data = &mut self.statics[id.0 as usize];
        let at = 8 * data.relocs.len();
        data.bytes.resize(data.bytes.len().max(at + 8), 0);
        data.relocs.push((at as u32, c));
    }

    /// `work` with the placeholders of `map` replaced by concrete types.
    fn subst_work(&mut self, work: &Work, map: &[(TyId, TyId)]) -> Work {
        let mut w = work.clone();
        match &mut w {
            Work::Checked(b) | Work::Read(b) | Work::ReadOwn(b) | Work::Deferred(b) => {
                **b = self.subst_work(b, map);
            }
            Work::Glue(_, t) => *t = self.replace_tys(*t, map),
            other => match super::super::program::work_target(other) {
                Some((_, a)) => {
                    let new: Vec<TyId> = a.clone();
                    *a = new.iter().map(|&t| self.replace_tys(t, map)).collect();
                }
                None => super::super::ice(format_args!(
                    "a dictionary entry for work of no type arguments: {work:?}"
                )),
            },
        }
        w
    }
}
