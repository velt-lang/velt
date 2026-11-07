//! Class vtable glue only for the operations the program performs.
//!
//! Every class with a vtable gets clone, format and transfer entries (`SLOT_CLONE`,
//! `SLOT_FORMAT`, `SLOT_TRANSFER`), and the entries keep their glue alive: a program that never
//! logs, clones or sends an object still compiled all three for every class. On the generated
//! 10k-line scale program that glue was a quarter of the release build's clang time.
//!
//! The only readers of these entries are the dispatches that clone, format and transfer an object
//! through its header (`glue/clone.rs`, `format.rs`, `many.rs`, `transfer.rs`), each on an object
//! of a known static class (the object may be of any subclass). They record the slot and class
//! with the function being built ([`FnLower::note_class_glue`]). Once the program is lowered,
//! [`Cx::prune_glue_slots`] walks what the program can reach from its exported functions,
//! following a class table's entry only when a reached function dispatches that slot on the
//! class or one of its bases. The glue itself dispatches on the classes of its fields, so a slot
//! the walk reaches can make more slots needed; the walk repeats until none is added. The
//! entries not followed become null, and `velt_opt`'s `dead_funcs` drops the glue no one else
//! calls. Interface and `any` tables keep all their entries: the runtime reads some of them
//! (`velt_rt::fn_props`).

use rustc_hash::{FxHashMap, FxHashSet};
use velt_sema::hir::{DefId, TyId, TyKind};

use super::{SLOT_CLONE, SLOT_FORMAT, SLOT_TRANSFER};
use crate::lower::{Cx, FnLower};
use crate::vir::{
    Callee, Const, FuncId, Function, Linkage, Operand, Rvalue, StaticId, Stmt, Terminator,
};

use super::vtable::HIDDEN;

/// The slots whose entries are pruned when unused.
const PRUNED: [i128; 3] = [SLOT_CLONE, SLOT_FORMAT, SLOT_TRANSFER];

/// What [`Cx::prune_glue_slots`] needs (module docs).
#[derive(Debug, Default)]
pub(in crate::lower) struct GlueSlots {
    /// Per function: the (slot, class) of every dispatch of a pruned slot it makes.
    uses: FxHashMap<FuncId, Vec<(i128, DefId)>>,
    /// Dispatches made outside any function build: needed whatever is reached.
    always: Vec<(i128, DefId)>,
    /// The functions being built, innermost last (builds nest).
    building: Vec<FuncId>,
    /// Every class table, with its class.
    tables: Vec<(StaticId, TyId)>,
}

impl GlueSlots {
    /// `fid` is being built.
    pub(in crate::lower) fn enter(&mut self, fid: FuncId) {
        self.building.push(fid);
    }

    /// The innermost build is done.
    pub(in crate::lower) fn leave(&mut self) {
        self.building.pop();
    }
}

impl Cx<'_> {
    /// Remember the table `id` of class `cls`, for [`Cx::prune_glue_slots`].
    pub(in crate::lower) fn note_class_table(&mut self, id: StaticId, cls: TyId) {
        self.glue_slots.tables.push((id, cls));
    }

    /// The class `cls` and its bases, as definitions.
    fn class_chain(&mut self, cls: TyId) -> Vec<DefId> {
        let mut chain = vec![];
        let mut cur = cls;
        while let TyKind::Adt(d, args) = self.kind(cur) {
            chain.push(d);
            match self.adt_def(d).base {
                Some(b) => cur = self.subst(b, &args),
                None => break,
            }
        }
        chain
    }

    /// Null the clone/format/transfer entries of class tables that no reachable code dispatches
    /// (module docs). Called once every function is built.
    pub(in crate::lower) fn prune_glue_slots(&mut self) {
        // The pruned entries: (table, byte offset) -> (slot, the class and its bases).
        let mut candidates: FxHashMap<(u32, u32), (i128, Vec<DefId>)> = FxHashMap::default();
        for (id, cls) in std::mem::take(&mut self.glue_slots.tables) {
            let chain = self.class_chain(cls);
            for slot in PRUNED {
                candidates.insert((id.0, (8 * (slot + HIDDEN)) as u32), (slot, chain.clone()));
            }
        }
        if candidates.is_empty() {
            return;
        }
        let funcs: Vec<&Function> = self.funcs.iter().flatten().collect();
        if funcs.len() != self.funcs.len() {
            return; // Not every function is built: keep everything.
        }
        let mut walk = Walk {
            live_funcs: vec![false; funcs.len()],
            live_statics: vec![false; self.statics.len()],
            todo_funcs: vec![],
            todo_statics: vec![],
        };
        // The roots: what the runtime and the linker reach by name.
        let externs: FxHashSet<&str> = self.externs.iter().map(|e| e.symbol.as_str()).collect();
        for (i, f) in funcs.iter().enumerate() {
            if f.linkage != Linkage::Internal || externs.contains(f.symbol.as_str()) {
                walk.func(i as u32);
            }
        }
        let mut needed: FxHashSet<(i128, DefId)> = self.glue_slots.always.iter().copied().collect();
        let mut waiting: Vec<(u32, u32, Const)> = vec![];
        let mut followed: FxHashSet<(u32, u32)> = FxHashSet::default();
        loop {
            while let Some(f) = walk.todo_funcs.pop() {
                let func = funcs[f as usize];
                for_each_const(func, |c| walk.reach(c));
                if let Some(uses) = self.glue_slots.uses.get(&FuncId(f)) {
                    needed.extend(uses.iter().copied());
                }
            }
            while let Some(s) = walk.todo_statics.pop() {
                for (at, c) in &self.statics[s as usize].relocs {
                    if candidates.contains_key(&(s, *at)) {
                        waiting.push((s, *at, c.clone()));
                    } else {
                        walk.reach(c);
                    }
                }
            }
            if !walk.todo_funcs.is_empty() || !walk.todo_statics.is_empty() {
                continue;
            }
            let before = followed.len();
            waiting.retain(|(s, at, c)| {
                let (slot, chain) = &candidates[&(*s, *at)];
                if chain.iter().any(|&d| needed.contains(&(*slot, d))) {
                    followed.insert((*s, *at));
                    walk.reach(c);
                    false
                } else {
                    true
                }
            });
            if followed.len() == before {
                break;
            }
        }
        for &(s, at) in candidates.keys() {
            if !followed.contains(&(s, at)) {
                self.statics[s as usize].relocs.retain(|(o, _)| *o != at);
            }
        }
    }
}

/// The functions and statics reached so far, and those still to look into.
struct Walk {
    live_funcs: Vec<bool>,
    live_statics: Vec<bool>,
    todo_funcs: Vec<u32>,
    todo_statics: Vec<u32>,
}

impl Walk {
    fn func(&mut self, f: u32) {
        if let Some(l) = self.live_funcs.get_mut(f as usize).filter(|l| !**l) {
            *l = true;
            self.todo_funcs.push(f);
        }
    }

    fn reach(&mut self, c: &Const) {
        match c {
            Const::Func(f) => self.func(f.0),
            Const::Static(s) => {
                if let Some(l) = self.live_statics.get_mut(s.0 as usize).filter(|l| !**l) {
                    *l = true;
                    self.todo_statics.push(s.0);
                }
            }
            _ => {}
        }
    }
}

/// Every constant `func` uses (operands, direct callees).
fn for_each_const(func: &Function, mut f: impl FnMut(&Const)) {
    let mut op = |o: &Operand| {
        if let Operand::Const(c, _) = o {
            f(c);
        }
    };
    for b in &func.blocks {
        for s in &b.stmts {
            match s {
                Stmt::Assign(_, rv) => match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => op(a),
                    Rvalue::Binary(_, a, c) => {
                        op(a);
                        op(c);
                    }
                    Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => {
                        ops.iter().for_each(&mut op)
                    }
                    Rvalue::AddrOf(_) => {}
                },
                Stmt::MemCopy { dst, src, .. } => {
                    op(dst);
                    op(src);
                }
                Stmt::MemCopyDyn { dst, src, len, .. } => {
                    op(dst);
                    op(src);
                    op(len);
                }
                Stmt::MemSet { dst, byte, len } => {
                    op(dst);
                    op(byte);
                    op(len);
                }
                Stmt::Nop => {}
            }
        }
        match &b.term {
            Terminator::Branch { cond, .. } => op(cond),
            Terminator::Switch { value, .. } => op(value),
            Terminator::Return(v) => op(v),
            Terminator::Call { callee, args, .. } => {
                match callee {
                    Callee::Func(g) => op(&Operand::Const(Const::Func(*g), crate::vir::Ty::Ptr)),
                    Callee::Ptr { target, .. } => op(target),
                    Callee::Extern(_) => {}
                }
                args.iter().for_each(&mut op);
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
}

impl FnLower<'_, '_> {
    /// [`FnLower::dispatch`] of a pruned slot of the table in the header of an object of class
    /// `cls` (or a subclass of it), noting the use (module docs).
    pub(in crate::lower) fn dispatch_class_glue(
        &mut self,
        vt: Operand,
        slot: i128,
        cls: TyId,
    ) -> Operand {
        self.note_class_glue(slot, cls);
        self.dispatch(vt, slot)
    }

    /// Note a dispatch of pruned slot `slot` on an object of class `cls` by the function being
    /// built.
    pub(in crate::lower) fn note_class_glue(&mut self, slot: i128, cls: TyId) {
        let TyKind::Adt(d, _) = self.cx.kind(cls) else {
            return;
        };
        let slots = &mut self.cx.glue_slots;
        match slots.building.last() {
            Some(&f) => slots.uses.entry(f).or_default().push((slot, d)),
            None => slots.always.push((slot, d)),
        }
    }
}
