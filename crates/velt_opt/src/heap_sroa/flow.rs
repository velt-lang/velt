//! The value-semantics check of `heap_sroa` (module docs there): a write through a web local
//! must be seen by every other local that may still read the same object.
//!
//! Three dataflow analyses over the locals of the remaining webs, at the granularity of
//! *events* (what a statement does to them, in execution order):
//! - **reads** (backward): a local is *live* where its current object may still be read or
//!   written through it (a write keeps the other fields), directly or through a local it is
//!   copied to later;
//! - **may-aliases** and **must-aliases** (forward, `aliases`): which other locals may hold the
//!   same object on some path, and which hold it on every path.
//!
//! At every write through `w`, every live alias of `w` must be a must-alias: the rewrite then
//! copies `w`'s object to those after the write ([`Update`]), so they see it as they would
//! through the heap. A live alias that holds the object only on some paths cannot be updated
//! (on the other paths it holds another object), so `w`'s web stays on the heap; so does a web
//! written by a call's result with a live alias (the copy would have to go after the call).

use velt_vir::vir::{Callee, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator};

use super::aliases::{self, bits, Aliases};
use super::webs::Webs;
use super::Allocator;
use crate::visit::{rvalue_operands, stmt_operands, successors, term_operands};

/// The analyses run on batches of webs with at most this many locals in all (one bit each);
/// a web with more locals stays on the heap.
const BATCH: usize = 64;

/// What a statement does to one web local (by its dense index).
#[derive(Clone, Copy)]
pub(super) enum Event {
    /// Reads the object (a field, or a pointer stored in it).
    Read(usize),
    /// Writes the object.
    Write(usize),
    /// The local now holds a new object, or none.
    Kill(usize),
    /// `dst = src`: both hold the same object.
    Copy { dst: usize, src: usize },
}

impl Event {
    /// The event renumbered by `slot` (dense index → batch index), if it is in the batch.
    fn renumber(self, slot: &[Option<usize>]) -> Option<Event> {
        Some(match self {
            Event::Read(i) => Event::Read(slot[i]?),
            Event::Write(i) => Event::Write(slot[i]?),
            Event::Kill(i) => Event::Kill(slot[i]?),
            Event::Copy { dst, src } => Event::Copy {
                dst: slot[dst]?,
                src: slot[src]?,
            },
        })
    }
}

/// After statement `stmt` of block `block` (a write through `src`), `dst` gets a copy of
/// `src`'s object: both hold the same object there, and `dst` is read later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Update {
    pub block: usize,
    pub stmt: usize,
    pub dst: Local,
    pub src: Local,
}

/// The events of one block, with the statement each comes from (`stmts.len()` for the
/// terminator).
#[derive(Default)]
struct BlockEvents {
    events: Vec<Event>,
    at: Vec<usize>,
}

/// Disqualify the webs whose value semantics would differ from reference semantics, and
/// return the updates that keep the others equal to it, in program order.
pub(super) fn check(allocator: Allocator, func: &Function, webs: &mut Webs) -> Vec<Update> {
    let index = Index::new(func, webs);
    if index.locals.is_empty() {
        return Vec::new();
    }
    let blocks: Vec<BlockEvents> = func
        .blocks
        .iter()
        .map(|b| index.block_events(allocator, &b.stmts, &b.term))
        .collect();
    let succs: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| successors(&b.term).iter().map(|s| s.0 as usize).collect())
        .collect();
    let mut updates = Vec::new();
    for batch in batches(&index, webs) {
        let mut slot = vec![None; index.locals.len()];
        for (k, &i) in batch.iter().enumerate() {
            slot[i] = Some(k);
        }
        let (events, at): (Vec<Vec<Event>>, Vec<Vec<usize>>) = blocks
            .iter()
            .map(|b| {
                let renumbered = b.events.iter().zip(&b.at);
                renumbered
                    .filter_map(|(ev, &at)| Some((ev.renumber(&slot)?, at)))
                    .unzip()
            })
            .unzip();
        let live_out = liveness(&events, &succs);
        let may_in = aliases::may(&events, &succs, batch.len());
        let must_in = aliases::must(&events, &succs, batch.len());
        let mut found = Vec::new();
        for (b, evs) in events.iter().enumerate() {
            let rows = Rows {
                may: may_in[b].clone(),
                must: must_in[b].clone(),
            };
            let term = func.blocks[b].stmts.len();
            let mut add = |at: usize, dst: usize, src: usize| {
                let (dst, src) = (index.locals[batch[dst]], index.locals[batch[src]]);
                found.push(Update {
                    block: b,
                    stmt: at,
                    dst,
                    src,
                });
            };
            for k in conflicts(evs, &at[b], term, live_out[b], rows, &mut add) {
                webs.disqualify(index.locals[batch[k]]);
            }
        }
        updates.extend(found);
    }
    updates.retain(|u| webs.obj(u.src).is_some());
    updates.sort_by_key(|u| (u.block, u.stmt));
    updates
}

/// The dense indices of the webs' locals, packed web by web into batches of at most
/// [`BATCH`]; webs too large for one batch are disqualified.
fn batches(index: &Index, webs: &mut Webs) -> Vec<Vec<usize>> {
    let mut by_web: Vec<Vec<usize>> = Vec::new();
    for (i, &l) in index.locals.iter().enumerate() {
        let web = webs.web(l).expect("ICE: heap_sroa local without a web") as usize;
        if by_web.len() <= web {
            by_web.resize(web + 1, Vec::new());
        }
        by_web[web].push(i);
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    for locals in by_web.into_iter().filter(|l| !l.is_empty()) {
        if locals.len() > BATCH {
            webs.disqualify(index.locals[locals[0]]);
            continue;
        }
        match out.last_mut() {
            Some(batch) if batch.len() + locals.len() <= BATCH => batch.extend(locals),
            _ => out.push(locals),
        }
    }
    out
}

/// Dense numbering of the locals of the remaining webs.
struct Index {
    of: Vec<Option<usize>>,
    locals: Vec<Local>,
}

impl Index {
    fn new(func: &Function, webs: &Webs) -> Index {
        let mut of = vec![None; func.locals.len()];
        let mut locals = Vec::new();
        for (i, slot) in of.iter_mut().enumerate() {
            if webs.obj(Local(i as u32)).is_some() {
                *slot = Some(locals.len());
                locals.push(Local(i as u32));
            }
        }
        Index { of, locals }
    }

    fn get(&self, l: Local) -> Option<usize> {
        self.of.get(l.0 as usize).copied().flatten()
    }

    /// The index of the local itself (no projection).
    fn whole(&self, p: &Place) -> Option<usize> {
        if p.proj.is_empty() {
            self.get(p.local)
        } else {
            None
        }
    }

    fn block_events(&self, allocator: Allocator, stmts: &[Stmt], term: &Terminator) -> BlockEvents {
        let mut out = BlockEvents::default();
        for (i, s) in stmts.iter().enumerate() {
            self.stmt_events(s, &mut out.events);
            out.at.resize(out.events.len(), i);
        }
        self.term_events(allocator, term, &mut out.events);
        out.at.resize(out.events.len(), stmts.len());
        out
    }

    fn read(&self, op: &Operand, out: &mut Vec<Event>) {
        if let Operand::Copy(p) = op {
            if let (Some(i), false) = (self.get(p.local), p.proj.is_empty()) {
                out.push(Event::Read(i));
            }
        }
    }

    /// A destination place through a web local: a write of the object, or a read of a pointer
    /// stored in it (when the place dereferences again).
    fn written(&self, p: &Place, out: &mut Vec<Event>) {
        let Some(i) = self.get(p.local) else { return };
        let nested = p.proj[1..].iter().any(|x| matches!(x, Proj::Deref(_)));
        out.push(if nested {
            Event::Read(i)
        } else {
            Event::Write(i)
        });
    }

    fn stmt_events(&self, s: &Stmt, out: &mut Vec<Event>) {
        match s {
            Stmt::Assign(dst, rv) => match self.whole(dst) {
                Some(d) => out.push(match rv {
                    Rvalue::Use(Operand::Copy(src)) => match self.whole(src) {
                        Some(src) => Event::Copy { dst: d, src },
                        None => Event::Kill(d),
                    },
                    _ => Event::Kill(d),
                }),
                None => {
                    rvalue_operands(rv, &mut |op| self.read(op, out));
                    self.written(dst, out);
                }
            },
            Stmt::MemSet {
                dst: Operand::Copy(p),
                ..
            } if self.whole(p).is_some() => {
                out.extend(self.whole(p).map(Event::Write));
            }
            _ => stmt_operands(s, &mut |op| self.read(op, out)),
        }
    }

    fn term_events(&self, allocator: Allocator, t: &Terminator, out: &mut Vec<Event>) {
        if let Terminator::Call {
            callee: Callee::Extern(e),
            args,
            dest,
            ..
        } = t
        {
            let dest = dest.as_ref().and_then(|d| self.whole(d));
            if let (true, Some(d)) = (*e == allocator.alloc, dest) {
                out.push(Event::Kill(d));
                return;
            }
            let freed = matches!(args.first(), Some(Operand::Copy(p)) if self.whole(p).is_some());
            if *e == allocator.free && freed {
                return;
            }
        }
        term_operands(t, &mut |op| self.read(op, out));
        if let Terminator::Call { dest: Some(d), .. } = t {
            self.written(d, out);
        }
    }
}

/// Backward transfer of one event over the live set (bit `i` = batch local `i`). A write
/// reads the rest of the object: the rewrite keeps the other fields of `w.obj` and may copy
/// all of it to the aliases, so `w.obj` must be current, not left stale by an earlier write
/// through an alias that skipped `w` because `w` was dead.
fn live_step(live: &mut u64, ev: Event) {
    match ev {
        Event::Read(i) | Event::Write(i) => *live |= 1 << i,
        Event::Kill(i) => *live &= !(1 << i),
        Event::Copy { dst, src } if dst != src => {
            let alive = *live & (1 << dst) != 0;
            *live &= !(1 << dst);
            if alive {
                *live |= 1 << src;
            }
        }
        Event::Copy { .. } => {}
    }
}

/// Live locals at the end of every block.
fn liveness(events: &[Vec<Event>], succs: &[Vec<usize>]) -> Vec<u64> {
    let blocks = events.len();
    let mut live_in = vec![0u64; blocks];
    let mut live_out = vec![0u64; blocks];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..blocks).rev() {
            let mut live = succs[b].iter().fold(0, |acc, &s| acc | live_in[s]);
            live_out[b] = live;
            for &ev in events[b].iter().rev() {
                live_step(&mut live, ev);
            }
            changed |= live & !live_in[b] != 0;
            live_in[b] |= live;
        }
    }
    live_out
}

/// The may- and must-alias rows at one point.
struct Rows {
    may: Aliases,
    must: Aliases,
}

/// Locals of one block written while an alias that cannot be updated is live; the updates of
/// the others go to `update(stmt, dst, src)`. `at[k]` is the statement of event `k`, `term`
/// the terminator's position.
fn conflicts(
    events: &[Event],
    at: &[usize],
    term: usize,
    live_out: u64,
    mut rows: Rows,
    update: &mut impl FnMut(usize, usize, usize),
) -> Vec<usize> {
    let mut live_after = vec![0u64; events.len()];
    let mut live = live_out;
    for (k, &ev) in events.iter().enumerate().rev() {
        live_after[k] = live;
        live_step(&mut live, ev);
    }
    let mut out = Vec::new();
    for (k, &ev) in events.iter().enumerate() {
        if let Event::Write(w) = ev {
            let seen = rows.may.0[w] & live_after[k] & !(1 << w);
            let updatable = if at[k] == term { 0 } else { rows.must.0[w] };
            if seen & !updatable != 0 {
                out.push(w);
            } else {
                for j in bits(seen) {
                    update(at[k], j, w);
                }
            }
        }
        rows.may.step(ev);
        rows.must.step(ev);
    }
    out
}
