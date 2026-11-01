//! Scalar replacement of enum aggregates: an enum local (a `Result` a throwing call returns, an
//! optional value) whose address is never taken and that is only read and written through its
//! tag and its variants' fields is split into one local per (offset, type) slot its variant
//! views use. After inlining, a throwing callee's `Ok` payload then reaches its caller in a
//! register, where the value-tracking passes see it: an element a `DataView` read is an
//! integer to `numrep`, not a float out of memory.
//!
//! `sroa` leaves enums alone because their payload is only reachable through variant views
//! (`Proj::Cast`), whose fields share bytes. Here each slot is its own local: a variant's
//! field is read only after the tag says that variant was written (the code lowering emits for
//! enums), so a read never needs the bits another variant's write left in shared bytes. A
//! candidate is used only as:
//! - `e.f…` (a field of the enum's own layout: the tag);
//! - `(e as V).f…` (a field of a variant view);
//!
//! and never whole (no copies of the whole value, no call returning into it, no address). Its
//! slots are zero-initialized on entry (VIR counts a field write as initializing the local).

use rustc_hash::FxHashMap;

use velt_vir::vir::{AggId, AggLayout, Function, Local, LocalDecl, Place, Proj, Stmt, Ty};

use crate::locals::Usage;
use crate::srclocs::prepend_stmts;
use crate::sroa::zero;
use crate::visit::{places_mut, stmt_places, term_places};

/// A slot of an enum local: the offset and type of the fields of its views that use it.
type Slot = (u32, Ty);

/// Split the enum locals of `func`; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let usage = Usage::of(func);
    // Per candidate: its slots, in order of first use.
    let mut cands: FxHashMap<Local, Vec<Slot>> = FxHashMap::default();
    let mut viewed: FxHashMap<Local, bool> = FxHashMap::default();
    for (i, decl) in func.locals.iter().enumerate().skip(func.params.len()) {
        let Ty::Agg(_) = decl.ty else { continue };
        let l = Local(i as u32);
        if !usage.get(l).address_taken {
            cands.insert(l, vec![]);
        }
    }
    if cands.is_empty() {
        return false;
    }
    let mut bad: Vec<Local> = vec![];
    let mut visit = |p: &Place| {
        let Some(slots) = cands.get_mut(&p.local) else {
            return;
        };
        match slot(aggs, &func.locals[p.local.0 as usize], p) {
            Some((s, view)) => {
                if view {
                    viewed.insert(p.local, true);
                }
                if !slots.contains(&s) {
                    slots.push(s);
                }
            }
            None => bad.push(p.local),
        }
    };
    for block in &func.blocks {
        for s in &block.stmts {
            stmt_places(s, &mut |p, _| visit(p));
        }
        term_places(&block.term, &mut |p, _| visit(p));
    }
    for l in bad {
        cands.remove(&l);
    }
    // Only enums (a local some view reads or writes), with scalar slots.
    cands.retain(|l, slots| {
        viewed.contains_key(l)
            && !slots.is_empty()
            && slots.len() <= crate::sroa::MAX_FIELDS
            && slots.iter().all(|(_, t)| t.scalar_size().is_some())
    });
    if cands.is_empty() {
        return false;
    }
    let mut map: FxHashMap<(Local, Slot), Local> = FxHashMap::default();
    let mut inits = vec![];
    let mut sorted: Vec<(&Local, &Vec<Slot>)> = cands.iter().collect();
    sorted.sort_by_key(|(l, _)| l.0);
    for (&e, slots) in sorted {
        let base = func.locals[e.0 as usize].name.clone();
        for &(off, ty) in slots {
            let l = Local(func.locals.len() as u32);
            let name = base.as_ref().map(|n| format!("{n}@{off}"));
            func.locals.push(LocalDecl {
                debug: None,
                ..LocalDecl::new(ty, name)
            });
            inits.push(Stmt::Assign(
                Place::local(l),
                velt_vir::vir::Rvalue::Use(zero(ty)),
            ));
            map.insert((e, (off, ty)), l);
        }
    }
    let locals = func.locals.clone();
    places_mut(func, &mut |p| {
        if !cands.contains_key(&p.local) {
            return;
        }
        let (s, _) = slot(aggs, &locals[p.local.0 as usize], p).expect("ICE: enum_sroa slot");
        let n = consumed(p);
        *p = Place {
            local: map[&(p.local, s)],
            proj: p.proj[n..].to_vec(),
        };
    });
    prepend_stmts(func, 0, inits);
    true
}

/// The slot place `p` of an enum local names, and whether it goes through a variant view;
/// `None` for a use the split cannot express (the whole value, a whole view).
fn slot(aggs: &[AggLayout], decl: &LocalDecl, p: &Place) -> Option<(Slot, bool)> {
    let Ty::Agg(own) = decl.ty else { return None };
    let (agg, field, view): (AggId, u32, bool) = match p.proj.as_slice() {
        [Proj::Field(f), ..] => (own, *f, false),
        [Proj::Cast(v), Proj::Field(f), ..] => (*v, *f, true),
        _ => return None,
    };
    let &(ty, off) = aggs.get(agg.0 as usize)?.fields.get(field as usize)?;
    Some(((off, ty), view))
}

/// How many projections of `p` the slot replaces.
fn consumed(p: &Place) -> usize {
    match p.proj.first() {
        Some(Proj::Cast(_)) => 2,
        _ => 1,
    }
}

#[cfg(test)]
#[path = "enum_sroa_tests.rs"]
mod tests;
