//! Rewriting a poll function so its chosen frame slots live in locals (see mod.rs).

use velt_vir::vir::{Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt, Terminator};

use super::scan::{overlaps, Frame, Range, SlotUse, Target};
use crate::noalias::{add_block, fresh_entry};
use crate::srclocs::{prepend_stmts, push_stmt, rewrite_stmts};
use crate::visit::{stmt_operands_mut, stmt_places, term_operands_mut, term_places, PlaceUse};

/// A promoted slot and its local.
struct Promoted {
    slot: SlotUse,
    local: Local,
}

/// Keep `slots` in new locals: loaded on entry, stored back before every `return` (the
/// suspension points), and stored / reloaded around the statements that access their bytes
/// through an aggregate or a reinterpreted view.
pub(super) fn promote(frame: &Frame, func: &mut Function, slots: Vec<SlotUse>) {
    let promoted: Vec<Promoted> = slots
        .into_iter()
        .map(|slot| {
            func.locals.push(LocalDecl {
                ty: slot.ty,
                name: None,
            });
            let local = Local(func.locals.len() as u32 - 1);
            Promoted { slot, local }
        })
        .collect();
    rename_slots(frame, func, &promoted);
    for bi in 0..func.blocks.len() {
        guard_statements(frame, func, bi, &promoted);
        guard_terminator(frame, func, bi, &promoted);
    }
    fresh_entry(func);
    let loads = promoted.iter().map(|p| load(frame, p)).collect();
    prepend_stmts(func, 0, loads);
}

/// `local = (*frame as State).path`
fn load(frame: &Frame, p: &Promoted) -> Stmt {
    let from = Operand::Copy(slot_place(frame, &p.slot.path));
    Stmt::Assign(Place::local(p.local), Rvalue::Use(from))
}

/// `(*frame as State).path = local`
fn store(frame: &Frame, p: &Promoted) -> Stmt {
    let from = Operand::Copy(Place::local(p.local));
    Stmt::Assign(slot_place(frame, &p.slot.path), Rvalue::Use(from))
}

fn slot_place(frame: &Frame, path: &[u32]) -> Place {
    let mut proj = vec![Proj::Deref(velt_vir::vir::Ty::Agg(frame.state))];
    proj.extend(path.iter().map(|n| Proj::Field(*n)));
    Place {
        local: frame.param,
        proj,
    }
}

/// Every read or write of a promoted slot names its local instead (the addresses taken to
/// define derived pointers stay: those pointers are only used through places renamed here).
fn rename_slots(frame: &Frame, func: &mut Function, promoted: &[Promoted]) {
    let rename = &mut |pl: &mut Place| {
        let Some(Target::Slot { path, end, .. }) = frame.target(pl) else {
            return;
        };
        if let Some(p) = promoted.iter().find(|p| p.slot.path == path) {
            let rest = pl.proj.split_off(end);
            *pl = Place {
                local: p.local,
                proj: rest,
            };
        }
    };
    for block in &mut func.blocks {
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut |op| {
                if let Operand::Copy(pl) = op {
                    rename(pl);
                }
            });
            if let Stmt::Assign(dst, _) = s {
                rename(dst);
            }
        }
        term_operands_mut(&mut block.term, &mut |op| {
            if let Operand::Copy(pl) = op {
                rename(pl);
            }
        });
        if let Terminator::Call { dest: Some(d), .. } = &mut block.term {
            rename(d);
        }
    }
}

/// Promoted slots overlapping the non-slot frame accesses reported to `visit`.
fn touched<'a>(
    frame: &Frame,
    promoted: &'a [Promoted],
    visit: impl FnOnce(&mut dyn FnMut(&Place, PlaceUse)),
) -> Vec<&'a Promoted> {
    let mut ranges: Vec<Range> = vec![];
    visit(&mut |pl, use_| {
        if use_ == PlaceUse::AddrOf || !frame.is_root(pl.local) {
            return;
        }
        if let Some(Target::Other(r)) = frame.target(pl) {
            ranges.push(r);
        }
    });
    promoted
        .iter()
        .filter(|p| ranges.iter().any(|&r| overlaps(r, p.slot.range)))
        .collect()
}

/// Around a statement that reads or writes promoted bytes as part of a bigger access: store
/// the written slots before it, reload all of them after it.
fn guard_statements(frame: &Frame, func: &mut Function, bi: usize, promoted: &[Promoted]) {
    let any = func.blocks[bi]
        .stmts
        .iter()
        .any(|s| !touched(frame, promoted, |v| stmt_places(s, &mut |p, u| v(p, u))).is_empty());
    if !any {
        return;
    }
    rewrite_stmts(func, bi, |s, out| {
        let hit = touched(frame, promoted, |v| stmt_places(&s, &mut |p, u| v(p, u)));
        out.extend(
            hit.iter()
                .filter(|p| p.slot.written)
                .map(|p| store(frame, p)),
        );
        out.push(s);
        out.extend(hit.iter().map(|p| load(frame, p)));
    });
}

/// Before a `return` store every written slot; before a terminator that reads promoted bytes
/// as part of a bigger access store the written ones it touches; after a call whose
/// destination covers promoted bytes reload those.
fn guard_terminator(frame: &Frame, func: &mut Function, bi: usize, promoted: &[Promoted]) {
    let term = &func.blocks[bi].term;
    let flush: Vec<&Promoted> = match term {
        Terminator::Return(_) => promoted.iter().collect(),
        t => touched(frame, promoted, |v| term_places(t, &mut |p, u| v(p, u))),
    };
    let reload: Vec<Stmt> = match term {
        Terminator::Call { dest: Some(d), .. } => {
            touched(frame, promoted, |v| v(d, PlaceUse::Write))
                .into_iter()
                .map(|p| load(frame, p))
                .collect()
        }
        _ => vec![],
    };
    let at = func.term_loc(bi);
    for p in flush.into_iter().filter(|p| p.slot.written) {
        push_stmt(func, bi, store(frame, p), at);
    }
    if reload.is_empty() {
        return;
    }
    if let Terminator::Call { next, .. } = func.blocks[bi].term {
        let reload_bb = add_block(func, reload, Terminator::Goto(next));
        if let Terminator::Call { next, .. } = &mut func.blocks[bi].term {
            *next = reload_bb;
        }
    }
}
