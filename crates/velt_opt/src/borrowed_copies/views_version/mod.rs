//! String views, versioned (#3469): after a demotion (`views.rs`) a family is static ASCII on the
//! view path, but the loop that reads it still tests the form on every step (`length` and
//! `charCodeAt` branch on it, `slice` chooses between its inline path and the runtime), and keeps
//! the member in memory because the runtime calls on the other paths take its address. Here the
//! region where the family is live is copied, and the view path runs the copy:
//!
//! - **The copy.** Every block of the region is copied, with every local the region defines
//!   renamed, so the copy's definitions can be told from the original's. The copy is entered only
//!   from the view block, through a block that copies the renamed locals in (`x' = x`; a pointer
//!   to a renamed local is taken again, `p' = &x'`), and each edge out of it goes back to the
//!   original code through a block that copies them out. The original code handles every form,
//!   so leaving the copy anywhere is correct.
//! - **The specialization.** In the copy, every member is assumed to be a static ASCII string:
//!   its form word (`w2`) reads as 0, and its count word (`w1`) reads as `(b << 32) | b` with `b`
//!   its byte count (31 bits), which LLVM sees has equal unit and byte counts. A member's drop is
//!   the zeroing a static string's drop is, and a clone of a member is a plain copy. Constant
//!   folding then removes the form tests and the paths they guarded: the runtime's slice, the
//!   non-ASCII reads, the drops of heap strings.
//! - **The proof.** The assumption holds when every definition of a member left in the copy
//!   makes a static ASCII string: a copy of a member, the entry copy of the demoted member (a
//!   view: its words were tested), a string aggregate whose form word is 0 and whose count word is
//!   `(n << 32) | n` (std's `sliceOf` builds its static slices so), or a zeroing (the empty
//!   string). By induction every member then holds a static ASCII string wherever the copy reads
//!   it, so the rewritten reads read what was there. Anything else (a call writing a member, a
//!   member's address passed to a call that is not read-only, a store through a pointer to one)
//!   fails the check, and the function is put back as it was.
//!
//! Each family with a single demotion is versioned, one after the other; a region with an
//! unwinding call, or a local holding a renamed local's address where the copy is entered or
//! left, is left alone, and so is a copy whose specialization removed no test and no call (it
//! would only add code: a function no copy pays for comes out exactly as it went in).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    AggLayout, BasicBlock, BlockId, Function, Local, Operand, Place, Rvalue, Stmt, Terminator,
};

use crate::visit::{successors, successors_mut};

use super::Runtime;

mod assigned;
mod proof;
mod renaming;
mod special;

use proof::proven;
use renaming::{rename_block, renaming};
use special::{fold_ascii, member_addresses, Special};

/// What versioning needs from the analysis of one family.
pub(super) struct Plan {
    /// The member the demotion writes.
    pub demoted: Local,
    /// The family's members.
    pub members: Vec<Local>,
    /// The blocks where the family is live (`Cx::live_blocks`), before any edit.
    pub region: Vec<usize>,
    /// The block the demotion continues to.
    pub next: usize,
}

/// Version `func` for each of `plans`, whose demotions' view blocks are given with them, then
/// clean up once. Each is tried on its own: a failed proof leaves the function as it was before
/// that one. The plans' block numbers stay valid throughout, because a versioning only adds
/// blocks and locals and constant folding renumbers neither; the cleanup runs last.
pub(super) fn version_all(
    aggs: &[AggLayout],
    rt: &Runtime,
    func: &mut Function,
    plans: &[(Plan, BlockId)],
) -> bool {
    let mut any = false;
    for (plan, view) in plans {
        any |= version(rt, func, plan, *view);
    }
    if any {
        crate::copyprop::run(func);
        crate::dce::run(aggs, func);
        crate::simplify_cfg::run(func);
    }
    any
}

/// Version `func` for `plan`, whose demotion's view block is `view`. Returns whether the
/// function changed; a failed proof leaves it as it was.
fn version(rt: &Runtime, func: &mut Function, plan: &Plan, view: BlockId) -> bool {
    let snapshot = func.clone();
    let Some(copy) = copy_region(rt, func, plan, view) else {
        *func = snapshot;
        return false;
    };
    fold_ascii(func, &copy);
    // Fold the form tests and the branches on them; locals and blocks keep their numbers.
    let sigs = [];
    let program = crate::constfold::Program::of(&sigs);
    crate::constfold::run(&program, func);
    crate::constfold::run(&program, func);
    // A copy the view path cannot reach (the demotion's test folded to the clone), or one whose
    // specialization removed no test and no call, would only add code: it is not taken.
    let entry = *copy
        .blocks
        .last()
        .expect("ICE: string views: a copy without an entry");
    let reachable = proof::reachable_blocks(func);
    let pays = tests_and_calls(func, &copy.region, &|_| true)
        > tests_and_calls(func, &copy.blocks, &|b| reachable[b]);
    if !reachable[entry] || !pays || !proven(rt, func, &copy) {
        *func = snapshot;
        return false;
    }
    true
}

/// What `copy_region` made: the renamed members, the demoted member's original, and each
/// canonical count word with its byte count (`ascii_w1`).
struct Copy {
    members: FxHashSet<Local>,
    demoted: Local,
    canon: FxHashMap<Local, Local>,
    /// The copied blocks, then the entry.
    blocks: Vec<usize>,
    /// The original blocks the copy was made from.
    region: Vec<usize>,
}

/// How many of `blocks` (those `keep` takes) end in a branch, a switch or a call.
fn tests_and_calls(func: &Function, blocks: &[usize], keep: &dyn Fn(usize) -> bool) -> usize {
    (blocks.iter().copied())
        .filter(|&b| keep(b))
        .filter(|&b| {
            matches!(
                func.blocks[b].term,
                Terminator::Branch { .. } | Terminator::Switch { .. } | Terminator::Call { .. }
            )
        })
        .count()
}

/// Copy the region of `plan` and specialize the copy; `None` when the region cannot be copied.
fn copy_region(rt: &Runtime, func: &mut Function, plan: &Plan, view: BlockId) -> Option<Copy> {
    let region = reachable_region(func, plan);
    let in_region = |b: usize| region.contains(&b);
    // An unwinding call would need its landing pad copied or a copy-out on the unwind edge.
    let unwinds = region.iter().any(|&b| {
        matches!(
            func.blocks[b].term,
            Terminator::Call {
                unwind: Some(_),
                ..
            }
        )
    });
    if unwinds {
        return None;
    }
    let renaming = renaming(rt, func, &region, plan.next)?;
    // The members the copy names: the renamed ones, and the demoted member itself when the
    // region never assigns it (it then holds the view all through the copy).
    let mut members: FxHashSet<Local> = (plan.members.iter())
        .filter(|m| renaming.fresh.contains_key(m))
        .map(|&m| renaming.local(m))
        .collect();
    members.insert(renaming.local(plan.demoted));
    let at = func.term_loc(view.0 as usize);
    let base = func.blocks.len() as u32;
    let new_id: FxHashMap<usize, BlockId> = (region.iter().enumerate())
        .map(|(k, &b)| (b, BlockId(base + k as u32)))
        .collect();
    // The copies, renamed, with their edges inside the region kept inside the copy.
    let mut exits: Vec<(usize, usize)> = vec![];
    let mut glue: Vec<usize> = vec![];
    for &b in &region {
        let mut block = func.blocks[b].clone();
        rename_block(&renaming, &mut block);
        successors_mut(&mut block.term, &mut |s| {
            let t = s.0 as usize;
            if let Some(&n) = new_id.get(&t) {
                *s = n;
            } else {
                exits.push((new_id[&b].0 as usize, t));
            }
        });
        let locs = func.locs.get(b).cloned();
        func.blocks.push(block);
        if let Some(l) = locs {
            func.locs.push(l);
        }
    }
    // Exits: a block copying out the renamed locals live at the target, then the target. An
    // address taken once is taken again; the other holders are dead there (`renaming`).
    for (from, to) in exits {
        let exit = BlockId(func.blocks.len() as u32);
        glue.push(exit.0 as usize);
        let mut stmts = vec![];
        let mut live: Vec<Local> = renaming.live_in[to].iter().copied().collect();
        live.sort_unstable_by_key(|l| l.0);
        for old in live {
            let new = renaming.local(old);
            match renaming.pointers.get(&old) {
                Some(target) => stmts.push(Stmt::Assign(
                    Place::local(old),
                    Rvalue::AddrOf(target.clone()),
                )),
                None if renaming.holders.contains(&old) => {}
                None => stmts.push(Stmt::Assign(
                    Place::local(old),
                    Rvalue::Use(Operand::Copy(Place::local(new))),
                )),
            }
        }
        push_block(func, stmts, Terminator::Goto(BlockId(to as u32)), at);
        successors_mut(&mut func.blocks[from].term, &mut |s| {
            if s.0 as usize == to && !in_region(to) {
                *s = exit;
            }
        });
    }
    // The entry: copy in the renamed locals live at `next`, then the copy of `next`.
    let mut stmts = vec![];
    let mut live: Vec<Local> = renaming.live_in[plan.next].iter().copied().collect();
    live.sort_unstable_by_key(|l| l.0);
    for old in live {
        let new = renaming.local(old);
        match renaming.pointers.get(&old) {
            Some(target) => {
                let mut t = target.clone();
                renaming.place(&mut t);
                stmts.push(Stmt::Assign(Place::local(new), Rvalue::AddrOf(t)));
            }
            None if renaming.holders.contains(&old) => {}
            None => stmts.push(Stmt::Assign(
                Place::local(new),
                Rvalue::Use(Operand::Copy(Place::local(old))),
            )),
        }
    }
    let entry = BlockId(func.blocks.len() as u32);
    push_block(func, stmts, Terminator::Goto(new_id[&plan.next]), at);
    match &mut func.blocks[view.0 as usize].term {
        Terminator::Goto(t) => *t = entry,
        _ => panic!("ICE: string views: the view block does not end in a jump"),
    }
    glue.push(entry.0 as usize);
    assigned::drop_unassigned_copies(func, &glue);
    // Specialize the copy.
    let copies: Vec<usize> = (base..base + region.len() as u32)
        .map(|b| b as usize)
        .collect();
    let (must, _) = member_addresses(func, &members);
    let spec = Special {
        rt,
        members: &members,
        pointers: must,
        canon: std::cell::RefCell::new(FxHashMap::default()),
    };
    for &b in &copies {
        spec.block(func, b);
    }
    let canon = spec.canon.into_inner();
    let mut blocks = copies;
    blocks.push(entry.0 as usize);
    Some(Copy {
        members,
        demoted: plan.demoted,
        canon,
        blocks,
        region,
    })
}

/// The region's blocks reachable from `next` without leaving it, in block order. Strong
/// liveness counts no drop as a use, so the block that drops a member's old value before the
/// loop gives it the new one is not live: the region also takes every block `next` dominates
/// that lies on a path from one of its blocks to another, or the copy would leave through that
/// hole for the original loop after one step. Only blocks `next` dominates: the way from the
/// loop's end back to a demotion run again (an outer loop) is not part of this run.
fn reachable_region(func: &Function, plan: &Plan) -> Vec<usize> {
    let n = func.blocks.len();
    let preds = crate::map_probe::region::predecessors(func);
    let doms = crate::map_probe::region::Dominators::new(func, &preds);
    let under: Vec<bool> = (0..n).map(|b| doms.dominates(plan.next, b)).collect();
    let mut live = vec![false; n];
    for &b in &plan.region {
        live[b] = true;
    }
    // Within the blocks `next` dominates: those after a live block, and those before one.
    let mut after = vec![false; n];
    let mut work: Vec<usize> = plan.region.clone();
    while let Some(b) = work.pop() {
        for s in successors(&func.blocks[b].term) {
            let s = s.0 as usize;
            if under[s] && !std::mem::replace(&mut after[s], true) {
                work.push(s);
            }
        }
    }
    let mut before = vec![false; n];
    let mut work: Vec<usize> = plan.region.clone();
    while let Some(b) = work.pop() {
        for &p in &preds[b] {
            if under[p] && !std::mem::replace(&mut before[p], true) {
                work.push(p);
            }
        }
    }
    let inside = |b: usize| live[b] || after[b] && before[b];
    let mut seen = vec![false; n];
    let mut order = vec![];
    let mut work = vec![plan.next];
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        order.push(b);
        for s in successors(&func.blocks[b].term) {
            let s = s.0 as usize;
            if inside(s) && !seen[s] {
                work.push(s);
            }
        }
    }
    order.sort_unstable();
    order
}

fn push_block(
    func: &mut Function,
    stmts: Vec<Stmt>,
    term: Terminator,
    at: Option<velt_vir::vir::SrcLoc>,
) {
    let n = stmts.len();
    func.blocks.push(BasicBlock { stmts, term });
    if !func.locs.is_empty() {
        func.locs.push(vec![at; n + 1]);
    }
}
