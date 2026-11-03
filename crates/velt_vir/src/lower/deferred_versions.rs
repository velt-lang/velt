//! View loops versioned once the counted types are complete (`lower_program`).
//!
//! A facts-only pass leaves a function's view loops unversioned (view_unswitch.rs): versioning
//! finds no shares, and it was most of a pass's time in large programs with array views. When
//! the pass that finds no more counted types is done, its functions are the program's but for
//! those versions, so each such function is finished then from the state its `finish` kept: its
//! loops are versioned and its sections placed, exactly as that `finish` would have.
//!
//! That is the same function, because nothing a version does reaches another function:
//! - Versioning reads the counted types (complete and the same then) and the function's own
//!   blocks and locals, and adds blocks and locals of its own.
//! - The one thing it adds to the program is the declaration of `velt_rt_array_view_cold`:
//!   `finish` declares it when the loops it chose will call it, so it gets the id it would have.
//! - The function's sections (epoch_sections.rs) are placed after versioning, as in `finish`;
//!   the `pin` and `unpin` functions they call were asked for there, so they have their ids.
//! - Other functions read a finished function only for its calls: the tuple-copy analysis
//!   (boxing/tuples.rs `callee_writes`) follows the call graph of the functions built so far.
//!   A version adds calls of nothing but `velt_rt_array_view_cold`, which calls nothing back,
//!   and keeps every call the loop had, so the analysis sees the same callees either way.
//! - Poll functions are always versioned in `finish`: their state layout (async_fn/spill) is
//!   computed from the versioned body.

use super::rt::Rt;
use super::view_unswitch::ViewLoops;
use super::{ice, Cx, FnLower, LkMode};
use crate::vir::{self, BasicBlock, BlockId, FuncId, LocalDecl, SrcLoc};
use velt_sema::hir::TyId;

/// A function's locals, its live blocks renumbered, and their locations.
pub(super) type Parts = (Vec<LocalDecl>, Vec<BasicBlock>, Vec<Vec<Option<SrcLoc>>>);

/// What a function's `finish` kept to version its view loops later: its state before them.
pub(super) struct Deferred {
    symbol: String,
    params: usize,
    targs: Vec<TyId>,
    mode: LkMode,
    ref_loads: bool,
    epoch_exempt: bool,
    locals: Vec<LocalDecl>,
    blocks: Vec<(Vec<vir::Stmt>, Option<vir::Terminator>)>,
    block_locs: Vec<(Vec<Option<SrcLoc>>, Option<SrcLoc>)>,
    live: Vec<bool>,
    view_loops: ViewLoops,
}

impl FnLower<'_, '_> {
    /// Keep this function's state to version its view loops once the counted types are
    /// complete (a facts-only pass), and declare what the versions will call.
    pub(super) fn defer_versions(&mut self, symbol: &str, params: usize) {
        let versions = self.view_loop_versions();
        if versions.is_empty() {
            return;
        }
        // A loop versioned for a box calls `velt_rt_array_view_cold` (view_unswitch.rs
        // `version`): declared here, it has the id it would have had.
        if versions.iter().any(|v| !v.boxes.is_empty()) {
            self.cx.rt(Rt::ArrayViewCold);
        }
        let d = Deferred {
            symbol: symbol.to_string(),
            params,
            targs: self.targs.clone(),
            mode: self.mode,
            ref_loads: self.ref_loads,
            epoch_exempt: self.epoch_exempt,
            locals: self.locals.clone(),
            blocks: self.blocks.clone(),
            block_locs: self.block_locs.clone(),
            live: self.live.clone(),
            view_loops: std::mem::take(&mut self.view_loops),
        };
        self.cx.deferred_new.push(d);
    }
}

impl Cx<'_> {
    /// The function `fid` was built: the state its `finish` kept (since `mark`) is its own.
    pub(super) fn claim_deferred(&mut self, fid: FuncId, mark: usize) {
        let mut mine = self.deferred_new.split_off(mark);
        match mine.len() {
            0 => {}
            1 => {
                self.deferred.insert(fid, mine.remove(0));
            }
            _ => ice("one build kept the state of two functions to version"),
        }
    }

    /// Version the view loops of the functions that left them (module docs).
    pub(super) fn version_deferred(&mut self) {
        if !self.deferred_new.is_empty() {
            ice("a function kept its state to version outside a build");
        }
        let mut all: Vec<(FuncId, Deferred)> =
            std::mem::take(&mut self.deferred).into_iter().collect();
        all.sort_by_key(|(f, _)| f.0);
        for (fid, d) in all {
            let mut lw = FnLower::bare(self, d.targs);
            lw.mode = d.mode;
            lw.ref_loads = d.ref_loads;
            lw.epoch_exempt = d.epoch_exempt;
            lw.locals = d.locals;
            lw.blocks = d.blocks;
            lw.block_locs = d.block_locs;
            lw.live = d.live;
            lw.view_loops = d.view_loops;
            lw.cur = BlockId(0);
            lw.loc = None;
            lw.cx.versioned += 1;
            lw.version_view_loops(d.params);
            lw.place_sections();
            let (locals, blocks, locs) = lw.into_parts(&d.symbol);
            let f = self.funcs[fid.0 as usize]
                .as_mut()
                .unwrap_or_else(|| ice("a deferred function was not built"));
            f.locals = locals;
            f.blocks = blocks;
            f.locs = locs;
        }
    }
}
