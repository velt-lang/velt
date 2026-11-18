//! Cleanup pads (docs/internals/design/unwinding.md "Cleanup in unwound frames", vir.rs
//! invariant 14): in a program that catches Velt panics, a call that may unwind in a frame that
//! owns values gets an unwind edge to a pad that drops what the open scopes own (their cancel
//! drops: no `finally`, no `catch`) and then continues the unwind (`velt_eh_resume`).
//!
//! A pad is a chain of links, one per drop entry, from the innermost to the outermost, each
//! jumping to the pad of the entries outside it; the empty pad calls `velt_eh_resume`. Links are
//! cached by the entries and the states they read, so calls in one ownership state share a pad
//! and a new temporary adds one link.

use rustc_hash::FxHashMap;
use velt_sema::hir::LocalId;

use super::state::{DropEntry, LState};
use super::FnLower;
use crate::eh;
use crate::vir::{self, BlockId};

/// The cleanup pads of the function being lowered.
#[derive(Default)]
pub(super) struct Pads {
    /// The first block of the pad for each chain of entries (by key, outermost first).
    cache: FxHashMap<String, BlockId>,
    /// The block calling `velt_eh_resume`, once built.
    resume: Option<BlockId>,
    /// In a poll or resume function: the block marking the state machine finished before the
    /// resume, once built.
    machine_resume: Option<BlockId>,
    /// While a pad is lowered: its calls get no unwind edge.
    inside: bool,
    /// What the frame owns outside the scopes while a drop is emitted, innermost: the entries
    /// of the scope `pop_scope` is dropping that are not dropped yet (it pops the scope first),
    /// and the new value of a local whose old value `drop_old` is dropping.
    pub(super) popped: Vec<DropEntry>,
    /// The locals whose old value `drop_old` is dropping: a pad doesn't drop them again.
    pub(super) dropping: Vec<LocalId>,
}

/// What the program's lowering knows of cleanup pads (`Cx::unwinding`).
#[derive(Default)]
pub(super) struct Unwinding {
    /// Whether calls get cleanup pads, once asked.
    on: Option<bool>,
}

impl super::Cx<'_> {
    /// Whether this program's calls get cleanup pads: it catches (sema demanded `panic_raise`,
    /// so `catchUnwind` is live), the target unwinds, and `VELT_UNWIND_PADS=0` (for measuring
    /// only) doesn't turn them off.
    pub(super) fn unwind_pads(&mut self) -> bool {
        if let Some(on) = self.unwinding.on {
            return on;
        }
        let index = velt_common::std_hooks::hook_index::panic_raise;
        let on = self.hir.hooks_demanded.contains(index)
            && eh::Catching::of_triple(&self.target) == eh::Catching::Itanium
            && std::env::var("VELT_UNWIND_PADS").map_or(true, |v| v != "0");
        self.unwinding.on = Some(on);
        on
    }
}

impl FnLower<'_, '_> {
    /// The unwind edge of a call of `callee` emitted now: the pad dropping what the frame owns,
    /// or `None` (the program doesn't catch, the callee can't unwind, the frame owns nothing,
    /// or this is a pad's own call). Every call in a state machine's body (an async function's
    /// poll, a generator's resume) that may unwind gets one: its pads drop what the state holds
    /// as what any frame owns, and end by marking the state finished (`machine_resume_block`).
    pub(super) fn cleanup_pad(&mut self, callee: &vir::Callee) -> Option<BlockId> {
        if self.pads.inside || self.dead() || !eh::may_unwind(callee) || !self.cx.unwind_pads() {
            return None;
        }
        // What the frame owns now: what an exit edge being emitted has dropped is not
        // (`Scope::pad_keep`), what `pop_scope` has not dropped yet still is (`popped`).
        let entries: Vec<DropEntry> = (self.scopes.iter())
            .flat_map(|s| s.drops.iter().take(s.pad_keep))
            .chain(&self.pads.popped)
            .filter(|d| self.drops_something(d))
            .cloned()
            .collect();
        // A state machine's pads end by marking it finished, also where the frame owns nothing.
        let machine = self.asyncx.is_some();
        if entries.is_empty() && !machine {
            return None;
        }
        let (cur, loc) = (self.cur, self.loc);
        let known = std::mem::take(&mut self.str_datas);
        self.pads.inside = true;
        let mut next = match machine {
            true => self.machine_resume_block(),
            false => self.resume_block(),
        };
        let mut key = String::new();
        for d in &entries {
            key.push_str(&self.entry_key(d));
            key.push(';');
            if let Some(&b) = self.pads.cache.get(&key) {
                next = b;
                continue;
            }
            let start = self.new_block();
            self.live[start.0 as usize] = true;
            self.switch_to(start);
            self.drop_entry(d);
            self.goto(next);
            self.pads.cache.insert(key.clone(), start);
            next = start;
        }
        self.pads.inside = false;
        self.str_datas = known;
        self.switch_to(cur);
        self.loc = loc;
        Some(next)
    }

    /// The block ending every pad: `velt_eh_resume()`.
    fn resume_block(&mut self) -> BlockId {
        if let Some(b) = self.pads.resume {
            return b;
        }
        let b = self.new_block();
        self.live[b.0 as usize] = true;
        self.switch_to(b);
        let (params, ret) = eh::resume_signature();
        let id = self.cx.extern_sym(eh::RESUME, params, ret, true);
        self.call(vir::Callee::Extern(id), vec![], None, true);
        self.pads.resume = Some(b);
        b
    }

    /// The block ending a state machine's pads: the state is marked finished (`mark_unwound`),
    /// its values dropped by the links before, then the unwind goes on. The frame that holds
    /// the state drops it as a finished one, which does nothing; a later `next()` of a
    /// generator is done.
    fn machine_resume_block(&mut self) -> BlockId {
        if let Some(b) = self.pads.machine_resume {
            return b;
        }
        let resume = self.resume_block();
        let b = self.new_block();
        self.live[b.0 as usize] = true;
        self.switch_to(b);
        self.mark_unwound();
        self.goto(resume);
        self.pads.machine_resume = Some(b);
        b
    }

    /// Whether dropping `d` now emits code (`drop_entry`): a local only when it is a cell, has
    /// a drop flag or is statically initialized; every other entry always.
    fn drops_something(&self, d: &DropEntry) -> bool {
        match d {
            DropEntry::Local(id) if self.pads.dropping.contains(id) => false,
            DropEntry::Local(id) => {
                let i = &self.info[id.0 as usize];
                i.cell || i.flag.is_some() || i.state == LState::Init
            }
            _ => true,
        }
    }

    /// What the code dropping `d` now depends on.
    fn entry_key(&self, d: &DropEntry) -> String {
        match d {
            DropEntry::Local(id) => {
                let i = &self.info[id.0 as usize];
                format!(
                    "L{}:{:?}:{:?}:{:?}:{}:{}",
                    id.0,
                    i.state,
                    i.flag,
                    i.moved_fields,
                    i.cell,
                    i.gen.is_some()
                )
            }
            DropEntry::Temp(p, ty) => format!("T{p}:{}", ty.0),
            DropEntry::HalfBuilt(p, ty) => format!("H{p}:{}", ty.0),
            DropEntry::Disposing(p, ty) => format!("D{p}:{}", ty.0),
            DropEntry::Flagged(p, ty, flag) => format!("F{p}:{}:{}", ty.0, flag.0),
            DropEntry::Rest(p, ty, pat) => {
                format!("R{p}:{}:{:p}", ty.0, std::rc::Rc::as_ptr(pat))
            }
            DropEntry::ConsumedArray { arr, next, elem } => {
                format!("C{arr}:{}:{}", next.0, elem.0)
            }
        }
    }
}
