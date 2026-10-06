//! The value-semantics check of `heap_sroa` (module docs there): a write through a web local
//! must not be observable through another local that may hold the same object.
//!
//! Two dataflow analyses over the locals of the remaining webs, at the granularity of
//! *events* (what a statement does to them, in execution order):
//! - **reads** (backward): a local is *live* where its current object may still be read
//!   through it, directly or through a local it is copied to later;
//! - **aliases** (forward, may): which other locals may hold the same object, created by
//!   copies and cleared by any other assignment.
//!
//! At every write through `w`, every alias of `w` must be dead; otherwise `w`'s web stays on
//! the heap.

use velt_vir::vir::{Callee, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator};

use super::webs::Webs;
use super::Allocator;
use crate::visit::{rvalue_operands, stmt_operands, successors, term_operands};

/// The analyses run on batches of webs with at most this many locals in all (one bit each);
/// a web with more locals stays on the heap.
const BATCH: usize = 64;

/// What a statement does to one web local (by its dense index).
#[derive(Clone, Copy)]
enum Event {
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

/// Disqualify the webs whose value semantics would differ from reference semantics.
pub(super) fn check(allocator: Allocator, func: &Function, webs: &mut Webs) {
    let index = Index::new(func, webs);
    if index.locals.is_empty() {
        return;
    }
    let events: Vec<Vec<Event>> = func
        .blocks
        .iter()
        .map(|b| index.block_events(allocator, &b.stmts, &b.term))
        .collect();
    let succs: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| successors(&b.term).iter().map(|s| s.0 as usize).collect())
        .collect();
    for batch in batches(&index, webs) {
        let mut slot = vec![None; index.locals.len()];
        for (k, &i) in batch.iter().enumerate() {
            slot[i] = Some(k);
        }
        let events: Vec<Vec<Event>> = events
            .iter()
            .map(|evs| evs.iter().filter_map(|ev| ev.renumber(&slot)).collect())
            .collect();
        let live_out = liveness(&events, &succs);
        let alias_in = aliases(&events, &succs, batch.len());
        for (b, evs) in events.iter().enumerate() {
            for k in conflicts(evs, live_out[b], &alias_in[b]) {
                webs.disqualify(index.locals[batch[k]]);
            }
        }
    }
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

    fn block_events(&self, allocator: Allocator, stmts: &[Stmt], term: &Terminator) -> Vec<Event> {
        let mut out = Vec::new();
        for s in stmts {
            self.stmt_events(s, &mut out);
        }
        self.term_events(allocator, term, &mut out);
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

/// Backward transfer of one event over the live set (bit `i` = batch local `i`).
fn live_step(live: &mut u64, ev: Event) {
    match ev {
        Event::Read(i) => *live |= 1 << i,
        Event::Write(_) => {}
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

/// May-alias rows: bit `j` of `rows[i]` says local `j` may hold `i`'s object.
#[derive(Clone, PartialEq)]
struct Aliases(Vec<u64>);

impl Aliases {
    fn forget(&mut self, i: usize) {
        let row = std::mem::take(&mut self.0[i]);
        for j in bits(row) {
            self.0[j] &= !(1 << i);
        }
    }

    fn step(&mut self, ev: Event) {
        match ev {
            Event::Kill(i) => self.forget(i),
            Event::Copy { dst, src } if dst != src => {
                self.forget(dst);
                let row = self.0[src];
                for j in bits(row) {
                    self.0[j] |= 1 << dst;
                }
                self.0[src] |= 1 << dst;
                self.0[dst] = row | (1 << src);
            }
            _ => {}
        }
    }

    fn union_with(&mut self, other: &Aliases) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            changed |= *b & !*a != 0;
            *a |= b;
        }
        changed
    }
}

/// The indices of the set bits.
fn bits(mut set: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (set != 0).then(|| {
            let i = set.trailing_zeros() as usize;
            set &= set - 1;
            i
        })
    })
}

/// Alias rows at the start of every block.
fn aliases(events: &[Vec<Event>], succs: &[Vec<usize>], n: usize) -> Vec<Aliases> {
    let mut alias_in = vec![Aliases(vec![0; n]); events.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (b, evs) in events.iter().enumerate() {
            let mut rows = alias_in[b].clone();
            for &ev in evs {
                rows.step(ev);
            }
            for &s in &succs[b] {
                changed |= alias_in[s].union_with(&rows);
            }
        }
    }
    alias_in
}

/// Locals of one block written while an alias is live.
fn conflicts(events: &[Event], live_out: u64, alias_in: &Aliases) -> Vec<usize> {
    let mut live_after = vec![0u64; events.len()];
    let mut live = live_out;
    for (k, &ev) in events.iter().enumerate().rev() {
        live_after[k] = live;
        live_step(&mut live, ev);
    }
    let mut rows = alias_in.clone();
    let mut out = Vec::new();
    for (k, &ev) in events.iter().enumerate() {
        if let Event::Write(w) = ev {
            if rows.0[w] & live_after[k] & !(1 << w) != 0 {
                out.push(w);
            }
        }
        rows.step(ev);
    }
    out
}
