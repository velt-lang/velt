//! Epoch sections (docs/internals/design/atomics.md "Sections"): a function that loads an
//! `Atomic` of a `shared` value (atomic_ref.rs) runs in a section of `velt/sync/epoch`, so a
//! value another thread retires meanwhile is not dropped while this one may still read it. The
//! section opens when the function starts (`pin`) and closes before each of its returns
//! (`unpin`). Sections nest, so a callee with loads of its own costs a depth count. In an async
//! function or a generator, whose code a suspension splits, each load is counted at once inside
//! a section of its own instead (atomic_ref.rs), so a section never spans an `await` or a
//! `yield`: a thread whose task is parked is never in one.
//!
//! The functions of `velt/sync/epoch` get none: they open sections, and the registry words they
//! load are never retired.

use super::FnLower;
use crate::vir::{BlockId, Callee, Terminator};

/// `velt/sync/epoch`'s functions, by their qualified names.
pub(super) const EPOCH_MODULE: &str = "std/sync/epoch/reclaim::";
pub(super) const PIN: &str = "std/sync/epoch/reclaim::pin";
pub(super) const UNPIN: &str = "std/sync/epoch/reclaim::unpin";

impl FnLower<'_, '_> {
    /// Opens a section at the entry and closes it before every return, when this function
    /// loads a reference `Atomic` (`ref_loads`) and is not `velt/sync/epoch`'s own.
    pub(super) fn place_sections(&mut self) {
        if !self.ref_loads || self.epoch_exempt {
            return;
        }
        let returns: Vec<BlockId> = (0..self.blocks.len())
            .filter(|&i| self.live[i])
            .filter(|&i| matches!(self.blocks[i].1, Some(Terminator::Return(_))))
            .map(|i| BlockId(i as u32))
            .collect();
        let unpin = self.epoch_fn(UNPIN);
        for b in returns {
            let Some(ret) = self.blocks[b.0 as usize].1.take() else {
                continue;
            };
            let loc = self.block_locs[b.0 as usize].1;
            self.switch_to(b);
            let saved = std::mem::replace(&mut self.loc, loc);
            self.call(Callee::Func(unpin), vec![], None, false);
            self.terminate(ret);
            self.loc = saved;
        }
        // The entry's code moves to a block of its own, behind the call opening the section.
        let body = self.new_block();
        let entry = std::mem::take(&mut self.blocks[0]);
        let entry_locs = std::mem::take(&mut self.block_locs[0]);
        let b = body.0 as usize;
        self.blocks[b] = entry;
        self.block_locs[b] = entry_locs;
        self.live[b] = true;
        for i in 0..self.blocks.len() {
            if let Some(t) = self.blocks[i].1.take() {
                let t = super::cfg::remap_term(t, |x| if x.0 == 0 { body } else { x });
                self.blocks[i].1 = Some(t);
            }
        }
        let pin = self.epoch_fn(PIN);
        self.switch_to(BlockId(0));
        let saved = self.loc.take();
        self.call(Callee::Func(pin), vec![], None, false);
        self.goto(body);
        self.loc = saved;
    }

    /// A call of `velt/sync/epoch`'s `pin` or `unpin` here.
    pub(super) fn epoch_call(&mut self, name: &str) {
        let f = self.epoch_fn(name);
        self.call(Callee::Func(f), vec![], None, false);
    }

    fn epoch_fn(&mut self, name: &str) -> crate::vir::FuncId {
        let d = self
            .cx
            .prelude_def(name)
            .unwrap_or_else(|| super::ice(format_args!("no `{name}`")));
        self.cx.func_for_direct(d, vec![], self.mode)
    }
}
