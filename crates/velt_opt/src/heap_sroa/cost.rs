//! Whether replacing a counted web pays: in locals, each copy of one of its pointers copies the
//! whole object (and its count), where on the heap it copied a pointer. An object made only on
//! a rare path (a `RangeError` that a hot loop carries a pointer for, null until something goes
//! wrong) is copied at every join of that loop, and it was never allocated there: `bench/pager`'s
//! decoder ran 24% more instructions with its error objects in locals. So a counted web whose
//! copies sit in deeper loops than any of its allocations stays on the heap.
//!
//! Only counted webs are judged (plain objects are replaced as they were before #1718), and only
//! in functions that have one, so the loops are found only there.

use velt_vir::vir::{Callee, Function, Local, Terminator};

use super::flow::Update;
use super::webs::{moved, Webs};
use super::Allocator;
use crate::invariant_reads::natural_loops;
use crate::map_probe::region::{predecessors, Dominators};
use velt_vir::vir::Stmt;

/// Keep on the heap the counted webs of `func` whose copies are in deeper loops than any of
/// their allocations.
pub(super) fn check(allocator: Allocator, func: &Function, webs: &mut Webs, updates: &[Update]) {
    let n = func.locals.len();
    let counted: Vec<Local> = (0..n as u32)
        .map(Local)
        .filter(|&l| webs.obj(l).is_some() && webs.counted(l))
        .collect();
    if counted.is_empty() {
        return;
    }
    let preds = predecessors(func);
    let doms = Dominators::new(func, &preds);
    let mut depth = vec![0u32; func.blocks.len()];
    match natural_loops(func, &preds, &doms) {
        Some(loops) => {
            for lp in &loops {
                for (d, _) in depth.iter_mut().zip(&lp.body).filter(|(_, &inside)| inside) {
                    *d += 1;
                }
            }
        }
        // An irreducible CFG: no loop depths to judge by, so keep the counted webs on the heap.
        None => {
            for l in counted {
                webs.disqualify(l);
            }
            return;
        }
    }
    // Per web: the deepest allocation and the deepest copy.
    let mut alloc_depth: Vec<Option<u32>> = vec![None; n];
    let mut copy_depth: Vec<u32> = vec![0; n];
    let web = |l: Local, webs: &Webs| webs.obj(l).and(webs.web(l)).map(|w| w as usize);
    for (bi, block) in func.blocks.iter().enumerate() {
        let d = depth[bi];
        for s in &block.stmts {
            let Stmt::Assign(dst, rv) = s else { continue };
            let Some((src, _)) = moved(rv) else { continue };
            if dst.proj.is_empty() && src.proj.is_empty() && dst.local != src.local {
                if let (Some(a), Some(b)) = (web(dst.local, webs), web(src.local, webs)) {
                    if a == b {
                        copy_depth[a] = copy_depth[a].max(d);
                    }
                }
            }
        }
        if let Terminator::Call {
            callee: Callee::Extern(e),
            dest: Some(dst),
            ..
        } = &block.term
        {
            if let (true, Some(w)) = (*e == allocator.alloc, web(dst.local, webs)) {
                let a = &mut alloc_depth[w];
                *a = Some(a.map_or(d, |a| a.max(d)));
            }
        }
    }
    for u in updates {
        if let Some(w) = web(u.dst, webs) {
            copy_depth[w] = copy_depth[w].max(depth[u.block]);
        }
    }
    for l in counted {
        if let Some(w) = web(l, webs) {
            if alloc_depth[w].is_some_and(|a| copy_depth[w] > a) {
                webs.disqualify(l);
            }
        }
    }
}
