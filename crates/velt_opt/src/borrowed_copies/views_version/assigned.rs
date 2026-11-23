//! The copies into and out of the copy that read a local some path leaves unassigned are left
//! out. The region's liveness (`renaming`) counts a write to a part of a local as a use of the
//! rest, so a local the region assigns part by part is live where the copy is entered, though
//! nothing assigned it before; the verifier, like the language, counts any write as assigning
//! it, and rejects the copy's read.
//!
//! Leaving such a copy out keeps every value a later read can see: when some path reaches the
//! copy with the local unassigned, a value from before the copy reaching a read after it would
//! make that path, continued to the read, a read of an unassigned local in the original
//! function, which the verifier rejects too.

use rustc_hash::FxHashSet;
use velt_vir::vir::{Function, Local, Operand, Rvalue, Stmt, Terminator};

/// Drop each whole-local copy in `blocks` whose source may be unassigned there, until none is.
pub(super) fn drop_unassigned_copies(func: &mut Function, blocks: &[usize]) {
    loop {
        let assigning = assigning_blocks(func);
        let preds = crate::map_probe::region::predecessors(func);
        let mut dropped = false;
        for &b in blocks {
            let mut here: FxHashSet<Local> = FxHashSet::default();
            for k in 0..func.blocks[b].stmts.len() {
                let Stmt::Assign(d, rv) = &func.blocks[b].stmts[k] else {
                    continue;
                };
                let source = match rv {
                    Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() && d.proj.is_empty() => {
                        Some(p.local)
                    }
                    _ => None,
                };
                if let Some(s) = source {
                    let param = (s.0 as usize) < func.params.len();
                    if !param && !here.contains(&s) && reaches_entry(&preds, &assigning, s, b) {
                        func.blocks[b].stmts[k] = Stmt::Nop;
                        dropped = true;
                        continue;
                    }
                }
                let Stmt::Assign(d, rv) = &func.blocks[b].stmts[k] else {
                    continue;
                };
                if !crate::visit::derefs(d) {
                    here.insert(d.local);
                }
                if let Rvalue::AddrOf(p) = rv {
                    if !crate::visit::derefs(p) {
                        here.insert(p.local);
                    }
                }
            }
        }
        if !dropped {
            return;
        }
    }
}

/// Per local, the blocks that assign it as the verifier counts: a write to it or a part of it,
/// its address taken, or a call's result put in it.
fn assigning_blocks(func: &Function) -> Vec<FxHashSet<usize>> {
    let mut out = vec![FxHashSet::default(); func.locals.len()];
    for (b, block) in func.blocks.iter().enumerate() {
        for s in &block.stmts {
            if let Stmt::Assign(d, rv) = s {
                if !crate::visit::derefs(d) {
                    out[d.local.0 as usize].insert(b);
                }
                if let Rvalue::AddrOf(p) = rv {
                    if !crate::visit::derefs(p) {
                        out[p.local.0 as usize].insert(b);
                    }
                }
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            if !crate::visit::derefs(d) {
                out[d.local.0 as usize].insert(b);
            }
        }
    }
    out
}

/// Whether a path from the entry reaches the start of block `b` without assigning `l`.
fn reaches_entry(preds: &[Vec<usize>], assigning: &[FxHashSet<usize>], l: Local, b: usize) -> bool {
    let assigns = &assigning[l.0 as usize];
    let mut seen: FxHashSet<usize> = FxHashSet::default();
    let mut work = vec![b];
    while let Some(x) = work.pop() {
        if x == 0 {
            return true;
        }
        for &p in &preds[x] {
            if !assigns.contains(&p) && seen.insert(p) {
                work.push(p);
            }
        }
    }
    false
}
