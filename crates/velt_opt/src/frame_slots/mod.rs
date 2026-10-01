//! Promotion of async frame slots into locals (FINDINGS 8.2, part 2).
//!
//! A local of an `async` function that lives across an `await` is a field of the state struct
//! (the *frame*), which the poll function `f$poll(state, cx)` reaches through its first param
//! (rt_abi_async.md §1). A hot loop between two awaits then loads and stores those fields on
//! every iteration, and LLVM cannot keep them in registers: the frame param carries no
//! `noalias`, and the loop calls functions (array growth, inlined child polls) that might,
//! for all it knows, reach the frame.
//!
//! **Why it is sound.** While a poll function runs, it owns its frame exclusively: the
//! executor (or the parent future) polls a state from one thread at a time and never touches it
//! during the poll, and before its first poll a state holds no pointers into itself (§1). So the
//! only code that can read or write a frame byte during the poll is (a) the poll function
//! through its param, (b) code given a pointer into the frame, and (c) code reading such a
//! pointer back from memory. This pass never promotes a byte that (b) or (c) could reach:
//! - the frame pointer itself may only be dereferenced; if it is copied, stored, offset or passed
//!   to any call, the function is left alone;
//! - a pointer to a part of the frame (`q = &(*state).f`) is followed only when `q` is assigned
//!   once and only dereferenced; if it is passed to a call or a memory copy, the part it
//!   points to is *exposed* and none of its slots is promoted (the callee may keep the pointer,
//!   like `velt_rt_all`'s result buffer); any other use of such an address leaves the function
//!   alone. Pointers stored in the frame were built by (b) in this same function, so every
//!   part they point to is exposed as well.
//!
//! **What is promoted.** A *slot* is a scalar field of the frame, named by its field path. A
//! slot is promoted when it is read inside a CFG cycle (a loop, where it matters), is not
//! exposed, and shares no byte with another slot (the state layout overlaps locals that are
//! never live together, see velt_vir's spill.rs). Suspension points are `return`s, so a promoted
//! slot is loaded on entry and stored back before every `return`; accesses to its bytes through
//! a whole aggregate or a reinterpreted view (the result region, an enum view) store it before
//! and reload it after. Aggregate writes to frame parts (`(*state).seq = agg { … }`) are split
//! into field writes first, so re-initializing an array in a loop keeps its fields promoted.
//!
//! Poll functions are the ones lowering marks (`Function::is_poll`); as a sanity check they
//! must also have the poll signature `(ptr, ptr) -> u32` (rt_abi_async.md §1) and dispatch
//! on the frame's tag in their entry block.
//!
//! **`noalias` on the frame.** When the scan finds that no pointer into the frame leaves the
//! function at all — the frame pointer and the derived pointers are only dereferenced (or
//! used by memory copies), never passed to a call, stored, returned or offset — the frame
//! param is marked `noalias`. Nothing else can then hold a pointer into the frame: this
//! function never handed one out, in this poll or an earlier one, and the caller hands the
//! frame over exclusively. Poll functions that pass a part of the frame on (a child's
//! `$poll`, `velt_rt_all`'s result buffer) do not get it: the callee may keep that pointer
//! and use it during a later poll, through code that does not receive the frame (the same
//! reason Rust emits no `noalias` for `Pin<&mut Self>` of a generator).

mod rewrite;
mod scan;

use velt_vir::vir::{
    AggId, AggLayout, Function, Local, Operand, ParamAttrs, Place, Proj, Rvalue, Stmt, Ty,
};

use crate::locals::Usage;
use crate::srclocs::rewrite_stmts;
use crate::visit::successors;
use scan::{overlaps, Facts, Frame, SlotUse};

/// Promote the frame slots of `func` if it is a poll function; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let Some(state) = poll_state(func) else {
        return false;
    };
    let mut frame = Frame {
        aggs,
        param: Local(0),
        state,
        derived: Default::default(),
    };
    frame.derived = scan::derived_pointers(&frame, func);
    let split = split_aggregate_writes(&frame, func);
    let edges: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| successors(&b.term).iter().map(|t| t.0 as usize).collect())
        .collect();
    let in_loop = crate::scc::on_cycle(&edges);
    let Some(facts) = scan::scan(&frame, func, &in_loop) else {
        return split;
    };
    let marked = !facts.passed_on && mark_frame_noalias(func);
    let slots = choose(facts);
    if slots.is_empty() {
        return split || marked;
    }
    rewrite::promote(&frame, func, slots);
    true
}

/// Add `noalias` to the frame param (module docs); returns whether it was new.
fn mark_frame_noalias(func: &mut Function) -> bool {
    if func.param_attrs.is_empty() {
        func.param_attrs = vec![ParamAttrs::default(); func.params.len()];
    }
    let frame = &mut func.param_attrs[0];
    !std::mem::replace(&mut frame.noalias, true)
}

/// The state aggregate of a poll function (`Function::is_poll`, signature `(state: ptr,
/// cx: ptr) -> u32`) that never reassigns `state` and whose entry block reads the tag
/// through it.
fn poll_state(func: &Function) -> Option<AggId> {
    if !func.is_poll
        || func.params != [Ty::Ptr, Ty::Ptr]
        || func.ret != Ty::U32
        || Usage::of(func).get(Local(0)).defs != 1
    {
        return None;
    }
    func.blocks.first()?.stmts.iter().find_map(|s| match s {
        Stmt::Assign(_, Rvalue::Use(Operand::Copy(pl))) if pl.local == Local(0) => {
            match pl.proj.first() {
                Some(Proj::Deref(Ty::Agg(a))) => Some(*a),
                _ => None,
            }
        }
        _ => None,
    })
}

/// Slots worth promoting and safe to promote (module docs).
fn choose(facts: Facts) -> Vec<SlotUse> {
    let keep: Vec<bool> = facts
        .slots
        .iter()
        .map(|s| {
            s.read
                && s.in_loop
                && !facts.exposed.iter().any(|&r| overlaps(r, s.range))
                && !facts
                    .slots
                    .iter()
                    .any(|o| o.path != s.path && overlaps(o.range, s.range))
        })
        .collect();
    facts
        .slots
        .into_iter()
        .zip(keep)
        .filter_map(|(s, k)| k.then_some(s))
        .collect()
}

/// `(*state).part = agg { a, b, … }` → `(*state).part.0 = a; (*state).part.1 = b; …` when the
/// operands do not read the frame (so the order of the field writes cannot matter).
fn split_aggregate_writes(frame: &Frame, func: &mut Function) -> bool {
    let splittable = |s: &Stmt| match s {
        Stmt::Assign(dst, Rvalue::Aggregate(id, ops)) => {
            frame.part_type(dst) == Some(Ty::Agg(*id))
                && ops.iter().all(|op| match op {
                    Operand::Copy(pl) => !frame.is_root(pl.local),
                    Operand::Const(..) => true,
                })
        }
        _ => false,
    };
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        if !func.blocks[bi].stmts.iter().any(splittable) {
            continue;
        }
        rewrite_stmts(func, bi, |s, out| {
            if !splittable(&s) {
                out.push(s);
                return;
            }
            let Stmt::Assign(dst, Rvalue::Aggregate(_, ops)) = s else {
                return;
            };
            for (k, op) in ops.into_iter().enumerate() {
                let mut place: Place = dst.clone();
                place.proj.push(Proj::Field(k as u32));
                out.push(Stmt::Assign(place, Rvalue::Use(op)));
            }
        });
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests;
