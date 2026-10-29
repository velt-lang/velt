//! Reads through object and array header pointers that no iteration can change are made once,
//! before the outermost loop around them that writes no memory.
//!
//! A typed array's `length` is computed from its buffer's bytes (`(*(*(*v).buf).bytes).len`)
//! at every `xs[i]`. LLVM keeps the chain inside a loop nest when the inner loop may run no
//! iteration (the reads are then not executed on every path) and it cannot prove the pointers
//! dereferenceable: the exit value of the inner loop's bounds check then lives in the outer
//! loop, `IndVarSimplify` keeps its exit, and the loop is neither freed of the check nor
//! vectorized (`bench/typed_array/loops.ts` `dot`). Here the chain is read once in front of
//! the loop nest, each pointer tested for null first, and LLVM sees values defined outside
//! every loop.
//!
//! A read `(*p as A).f…` qualifies when, for the loop it leaves (and every loop between):
//! - **nothing in the loop writes memory**: no store through a pointer, into a local whose
//!   address is taken, or by `memcpy`/`memset`, and no call but SIMD operations that only
//!   compute or load (no store, no optimistic load of atomic words). A free is a call, so no
//!   object is freed either, and no other thread changes a plain field (sema's exclusivity; an
//!   `Atomic`'s word, which may change, is never read here);
//! - **the pointers are object or header pointers**: `p` is a pointer local the loop does not
//!   assign (or assigns only copies of one such local, a copy that dominates the read), every
//!   `Deref` names a class object or the array header, every pointer followed after the first
//!   is a field of a class object (never an array's data pointer: element memory is checked by
//!   index, which a read in front of the loop would skip), and no `Cast` (an enum view) or
//!   `Atomic` aggregate is on the way.
//!
//! The reads in front of the loop may run where the loop's own would not (no iteration, or a
//! branch inside the loop not taken), so they must be safe to make anyway:
//! - each pointer is tested for null before it is followed: a `T | null` field, or a field of
//!   an object whose constructor has not set it yet (the zero fill);
//! - a non-null object or header pointer held by a local the loop reads is valid in front of
//!   it: a local holds an owned, counted or borrowed reference, and sema keeps a borrowed
//!   object alive (or rejects the program) while any later use of the local may run, whatever
//!   the branches, since its analysis does not follow path conditions; the loop's read is such
//!   a later use. A header pointer is an array value's own (a local, a field, a parameter),
//!   never one computed from an element index;
//! - a non-null pointer in a field of a valid object is a reference the object holds (owned or
//!   counted; a weak reference is a slot number the runtime resolves, not a pointer), so it is
//!   valid too. Nothing in the loop frees anything (no call), and the reads in front of it
//!   run right before it.
//!
//! Where the loop's read does run, its pointers are non-null (Velt never reads through null),
//! so the value read in front of the loop is the one it reads: nothing writes in between. A
//! pointer into an array's elements is never followed: its validity depends on an index the
//! loop may check.
//!
//! `!dereferenceable` metadata on the loads would not do this in LLVM: LICM drops it from a
//! load it moves to where the load may not run, and the next load of the chain then cannot
//! move either; and an object field may be null (the zero fill) where LLVM would assume not.

use std::collections::HashMap;

use velt_vir::simd::{Op, SimdCall};
use velt_vir::vir::{
    AggId, AggLayout, BasicBlock, BinOp, BlockId, Callee, Const, ExternFn, Function, Local,
    LocalDecl, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::locals::{place_ty, Usage};
use crate::map_probe::region::{predecessors, Dominators};
use crate::noalias::add_block;
use crate::visit::{
    stmt_operands, stmt_operands_mut, successors, successors_mut, term_operands, term_operands_mut,
};

/// Hoist the qualifying reads of `func`'s loops; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], kinds: &Kinds, func: &mut Function) -> bool {
    let Some(reads) = chain_reads(aggs, kinds, func) else {
        return false;
    };
    let preds = predecessors(func);
    let doms = Dominators::new(func, &preds);
    let Some(loops) = natural_loops(func, &preds, &doms) else {
        return false;
    };
    if loops.is_empty() {
        return false;
    }
    let usage = Usage::of(func);
    // Only loops with a chain read matter (every loop around one has it too).
    let facts: Vec<LoopFacts> = loops
        .iter()
        .map(|lp| {
            let relevant = lp.body.iter().zip(&reads).any(|(b, r)| *b && *r);
            if relevant {
                LoopFacts::of(kinds, func, &usage, lp)
            } else {
                LoopFacts {
                    quiet: false,
                    defs: Defs::new(),
                }
            }
        })
        .collect();
    let cx = Cx {
        aggs,
        kinds,
        func,
        doms: &doms,
        loops: &loops,
        facts: &facts,
    };
    // Per loop (index into `loops`): the places read in front of it, and per operand that
    // reads one (block, operand number in the block's visiting order): which.
    let mut hoists: Vec<Vec<Place>> = vec![vec![]; loops.len()];
    let mut uses: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        if !reads[bi] {
            continue;
        }
        let around: Vec<usize> = cx.loops_around(bi);
        if around.is_empty() {
            continue;
        }
        let mut k = 0;
        let mut visit = |op: &Operand, at: usize| {
            if let Some((lp, place)) = cx.target(&around, bi, at, op) {
                let list = &mut hoists[lp];
                let i = match list.iter().position(|p| *p == place) {
                    Some(i) => i,
                    None => {
                        list.push(place);
                        list.len() - 1
                    }
                };
                uses.insert((bi, k), (lp, i));
            }
            k += 1;
        };
        for (si, s) in block.stmts.iter().enumerate() {
            stmt_operands(s, &mut |op| visit(op, si));
        }
        term_operands(&block.term, &mut |op| visit(op, block.stmts.len()));
    }
    if uses.is_empty() {
        return false;
    }
    // The locals that hold the reads, then the reads in front of each loop.
    let mut locals: Vec<Vec<Local>> = vec![];
    for places in &hoists {
        let mut ls = vec![];
        for p in places {
            let ty = place_ty(aggs, func, p).expect("ICE: hoisted read has a type");
            ls.push(Local(func.locals.len() as u32));
            func.locals.push(LocalDecl::new(ty, None));
        }
        locals.push(ls);
    }
    for (li, places) in hoists.iter().enumerate() {
        if !places.is_empty() {
            read_in_front(func, &loops[li], places, &locals[li]);
        }
    }
    for bi in 0..func.blocks.len() {
        let mut k = 0;
        let mut rewrite = |op: &mut Operand| {
            if let Some(&(lp, i)) = uses.get(&(bi, k)) {
                *op = Operand::Copy(Place::local(locals[lp][i]));
            }
            k += 1;
        };
        let block = &mut func.blocks[bi];
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut rewrite);
        }
        term_operands_mut(&mut block.term, &mut rewrite);
    }
    true
}

/// What an aggregate is to the reads here.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A class object (`velt_vir::OBJECT_AGG_SUFFIX`).
    Object,
    /// The array header (`velt_vir::ARRAY_AGG_NAME`).
    Header,
    /// An `Atomic`, whose word other threads may store to.
    Atomic,
    Other,
}

/// What the program's aggregates and externs are to the pass, computed once.
pub(crate) struct Kinds {
    /// The [`Kind`] of every aggregate.
    aggs: Vec<Kind>,
    /// Per extern: whether a call only computes values ([`computes_only`]).
    computes: Vec<bool>,
}

impl Kinds {
    pub(crate) fn of(aggs: &[AggLayout], externs: &[ExternFn]) -> Kinds {
        Kinds {
            aggs: aggs
                .iter()
                .map(|a| match a.name.as_str() {
                    velt_vir::ARRAY_AGG_NAME => Kind::Header,
                    "Atomic" => Kind::Atomic,
                    n if n.ends_with(velt_vir::OBJECT_AGG_SUFFIX) => Kind::Object,
                    _ => Kind::Other,
                })
                .collect(),
            computes: externs.iter().map(computes_only).collect(),
        }
    }

    fn get(&self, id: AggId) -> Kind {
        self.aggs.get(id.0 as usize).copied().unwrap_or(Kind::Other)
    }

    /// Whether a call of `callee` only computes values.
    fn computes(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Extern(id) if self.computes.get(id.0 as usize) == Some(&true))
    }
}

/// Per block, whether it reads a chain; `None` when no block that may be on a loop writing no
/// memory does: most functions have no such loop, and the dominators and loops are not worth
/// computing for them.
fn chain_reads(aggs: &[AggLayout], kinds: &Kinds, func: &Function) -> Option<Vec<bool>> {
    // A cycle has an edge to a block numbered no higher than its source.
    let backwards = func.blocks.iter().enumerate().any(|(bi, b)| {
        let mut back = false;
        for_each_target(&b.term, |t| back |= t <= bi);
        back
    });
    if !backwards {
        return None;
    }
    // The blocks that may be on a cycle of blocks writing no memory (address-taken locals
    // aside, which `LoopFacts` checks): drop the blocks without an edge in or out among the
    // rest until none is left to drop; every such cycle stays.
    let n = func.blocks.len();
    let mut alive: Vec<bool> = func.blocks.iter().map(|b| calm(kinds, b)).collect();
    let (mut ins, mut outs) = (vec![0u32; n], vec![0u32; n]);
    for (u, b) in func.blocks.iter().enumerate().filter(|(u, _)| alive[*u]) {
        for_each_target(&b.term, |v| {
            if v < n && alive[v] {
                outs[u] += 1;
                ins[v] += 1;
            }
        });
    }
    // The predecessors among them, flat: those of `v` are `preds[start[v]..start[v + 1]]`.
    let mut start = vec![0usize; n + 1];
    for v in 0..n {
        start[v + 1] = start[v] + ins[v] as usize;
    }
    let mut preds = vec![0usize; start[n]];
    let mut fill = start.clone();
    for (u, b) in func.blocks.iter().enumerate().filter(|(u, _)| alive[*u]) {
        for_each_target(&b.term, |v| {
            if v < n && alive[v] {
                preds[fill[v]] = u;
                fill[v] += 1;
            }
        });
    }
    let mut work: Vec<usize> = (0..n)
        .filter(|&b| alive[b] && (ins[b] == 0 || outs[b] == 0))
        .collect();
    while let Some(b) = work.pop() {
        if !std::mem::replace(&mut alive[b], false) {
            continue;
        }
        for_each_target(&func.blocks[b].term, |v| {
            if v < n && alive[v] {
                ins[v] -= 1;
                if ins[v] == 0 {
                    work.push(v);
                }
            }
        });
        for &u in &preds[start[b]..start[b + 1]] {
            if alive[u] {
                outs[u] -= 1;
                if outs[u] == 0 {
                    work.push(u);
                }
            }
        }
    }
    // Of those, the ones that read a chain.
    let mut any = false;
    for (b, block) in func.blocks.iter().enumerate() {
        if !alive[b] {
            continue;
        }
        let mut found = false;
        let mut seen = |op: &Operand| {
            found = found || matches!(op, Operand::Copy(p) if chain(aggs, kinds, func, p));
        };
        block.stmts.iter().for_each(|s| stmt_operands(s, &mut seen));
        term_operands(&block.term, &mut seen);
        alive[b] = found;
        any |= found;
    }
    any.then_some(alive)
}

/// Whether a block writes no memory through a pointer and calls only computing SIMD
/// operations.
fn calm(kinds: &Kinds, b: &BasicBlock) -> bool {
    let deref = |p: &Place| p.proj.iter().any(|x| matches!(x, Proj::Deref(_)));
    b.stmts.iter().all(|s| match s {
        Stmt::Assign(dest, _) => !deref(dest),
        Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => false,
        Stmt::Nop => true,
    }) && match &b.term {
        Terminator::Call { callee, dest, .. } => {
            kinds.computes(callee) && !dest.as_ref().is_some_and(deref)
        }
        _ => true,
    }
}

/// Visit the successors of a terminator (with duplicates), without allocating.
fn for_each_target(t: &Terminator, mut f: impl FnMut(usize)) {
    match t {
        Terminator::Goto(b) | Terminator::Call { next: b, .. } => f(b.0 as usize),
        Terminator::Branch { then, els, .. } => {
            f(then.0 as usize);
            f(els.0 as usize);
        }
        Terminator::Switch { cases, default, .. } => {
            cases.iter().for_each(|(_, b)| f(b.0 as usize));
            f(default.0 as usize);
        }
        Terminator::Return(_) | Terminator::Unreachable => {}
    }
}

/// Whether `place` reads a scalar through object or header pointers only (module doc).
fn chain(aggs: &[AggLayout], kinds: &Kinds, func: &Function, place: &Place) -> bool {
    if !matches!(place.proj.first(), Some(Proj::Deref(_))) {
        return false;
    }
    let Some(mut ty) = func.locals.get(place.local.0 as usize).map(|l| l.ty) else {
        return false;
    };
    // Whether the pointer being followed is a field of a class object (or the root).
    let mut in_object = true;
    for p in &place.proj {
        ty = match (p, ty) {
            (Proj::Deref(Ty::Agg(id)), Ty::Ptr) => {
                let kind = kinds.get(*id);
                if !in_object || !matches!(kind, Kind::Object | Kind::Header) {
                    return false;
                }
                in_object = kind == Kind::Object;
                Ty::Agg(*id)
            }
            (Proj::Field(n), Ty::Agg(id)) => {
                match kinds.get(id) {
                    Kind::Atomic => return false,
                    // An array header inline in an object: its data pointer is not followed.
                    Kind::Header => in_object = false,
                    Kind::Object | Kind::Other => {}
                }
                match aggs
                    .get(id.0 as usize)
                    .and_then(|a| a.fields.get(*n as usize))
                {
                    Some(&(t, _)) => t,
                    None => return false,
                }
            }
            _ => return false,
        };
    }
    ty.is_scalar() && ty != Ty::V128
}

/// A natural loop: a head and the blocks that reach one of its latches without passing it.
pub(super) struct Loop {
    head: usize,
    body: Vec<bool>,
    size: usize,
}

/// The natural loops of `func`, one per head; `None` when the CFG is irreducible.
fn natural_loops(func: &Function, preds: &[Vec<usize>], doms: &Dominators) -> Option<Vec<Loop>> {
    let n = func.blocks.len();
    let reachable = |b: usize| doms.dominates(b, b);
    let mut latches: Vec<Vec<usize>> = vec![vec![]; n];
    for u in (0..n).filter(|&u| reachable(u)) {
        for v in successors(&func.blocks[u].term) {
            let v = v.0 as usize;
            if v < n && reachable(v) && doms.rank(v) <= doms.rank(u) {
                if !doms.dominates(v, u) {
                    return None;
                }
                latches[v].push(u);
            }
        }
    }
    let mut loops = vec![];
    for (head, latches) in latches.into_iter().enumerate() {
        if latches.is_empty() {
            continue;
        }
        let mut body = vec![false; n];
        body[head] = true;
        let mut work = latches;
        while let Some(b) = work.pop() {
            if !std::mem::replace(&mut body[b], true) {
                work.extend(preds[b].iter().copied().filter(|&p| reachable(p)));
            }
        }
        let size = body.iter().filter(|&&b| b).count();
        loops.push(Loop { head, body, size });
    }
    Some(loops)
}

/// Per local a loop assigns: `Some((src, sites))` when every assignment in it is a copy of the
/// local `src` (made at the statements `sites`), `None` for any other assignment.
type Defs = HashMap<u32, Option<(Local, Vec<(usize, usize)>)>>;

/// What one loop writes.
struct LoopFacts {
    /// Nothing in the loop writes memory or calls anything but a computing SIMD operation.
    quiet: bool,
    /// The locals the loop assigns.
    defs: Defs,
}

impl LoopFacts {
    fn of(kinds: &Kinds, func: &Function, usage: &Usage, lp: &Loop) -> LoopFacts {
        let mut quiet = true;
        let mut defs = Defs::new();
        let writes_memory = |p: &Place| {
            p.proj.iter().any(|x| matches!(x, Proj::Deref(_))) || usage.get(p.local).address_taken
        };
        for (bi, block) in func.blocks.iter().enumerate() {
            if !lp.body[bi] {
                continue;
            }
            for (si, s) in block.stmts.iter().enumerate() {
                match s {
                    Stmt::Assign(dest, rv) => {
                        quiet &= !writes_memory(dest);
                        let copy = match rv {
                            Rvalue::Use(Operand::Copy(src))
                                if dest.proj.is_empty() && src.proj.is_empty() =>
                            {
                                Some(src.local)
                            }
                            _ => None,
                        };
                        note_def(&mut defs, dest, copy, (bi, si));
                    }
                    Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => {
                        quiet = false;
                    }
                    Stmt::Nop => {}
                }
            }
            if let Terminator::Call { callee, dest, .. } = &block.term {
                quiet &= kinds.computes(callee);
                if let Some(d) = dest {
                    quiet &= !writes_memory(d);
                    note_def(&mut defs, d, None, (bi, block.stmts.len()));
                }
            }
        }
        LoopFacts { quiet, defs }
    }
}

fn note_def(defs: &mut Defs, dest: &Place, copy: Option<Local>, at: (usize, usize)) {
    let slot = defs
        .entry(dest.local.0)
        .or_insert_with(|| copy.map(|c| (c, vec![])));
    match (slot.as_mut(), copy, dest.proj.is_empty()) {
        (Some((src, sites)), Some(c), true) if *src == c => sites.push(at),
        _ => *slot = None,
    }
}

/// Whether a call of `ext` only computes values (or loads them): a SIMD operation other than
/// a store or an optimistic load of words other threads may store to.
fn computes_only(ext: &ExternFn) -> bool {
    match SimdCall::of_symbol(&ext.symbol) {
        Some(SimdCall::Vector(_, op)) => {
            !matches!(op, Op::Store | Op::StorePartial | Op::LoadOptimistic)
        }
        Some(SimdCall::Mask(..) | SimdCall::Has(_) | SimdCall::Prefetch) => true,
        Some(SimdCall::Inst(_)) | None => false,
    }
}

struct Cx<'a> {
    aggs: &'a [AggLayout],
    kinds: &'a Kinds,
    func: &'a Function,
    doms: &'a Dominators,
    loops: &'a [Loop],
    facts: &'a [LoopFacts],
}

impl Cx<'_> {
    /// The loops around block `b`, innermost first.
    fn loops_around(&self, b: usize) -> Vec<usize> {
        let mut around: Vec<usize> = (0..self.loops.len())
            .filter(|&i| self.loops[i].body[b])
            .collect();
        around.sort_by_key(|&i| self.loops[i].size);
        around
    }

    /// For an operand of block `b` at statement `at` (the terminator: the statement count):
    /// the outermost loop around it to read it in front of, and the place to read there.
    fn target(
        &self,
        around: &[usize],
        b: usize,
        at: usize,
        op: &Operand,
    ) -> Option<(usize, Place)> {
        let Operand::Copy(place) = op else {
            return None;
        };
        if !chain(self.aggs, self.kinds, self.func, place) {
            return None;
        }
        let mut best = None;
        for &li in around {
            // The block-0 head has no edge from outside to give a block in front.
            if !self.facts[li].quiet || self.loops[li].head == 0 {
                break;
            }
            let Some(root) = self.resolve(li, place.local, (b, at), 0) else {
                break;
            };
            best = Some((
                li,
                Place {
                    local: root,
                    proj: place.proj.clone(),
                },
            ));
        }
        best
    }

    /// The local outside loop `li` that `l` holds at `at` in it: `l` itself when the loop does
    /// not assign it, or the source of its copies when they are all of one local and one of
    /// them dominates `at`.
    fn resolve(&self, li: usize, l: Local, at: (usize, usize), depth: u32) -> Option<Local> {
        if self.func.locals.get(l.0 as usize)?.ty != Ty::Ptr {
            return None;
        }
        let Some(def) = self.facts[li].defs.get(&l.0) else {
            return Some(l);
        };
        let (src, sites) = def.as_ref()?;
        if depth > 8 || *src == l {
            return None;
        }
        let site = sites
            .iter()
            .find(|&&(db, ds)| self.before(db, ds, at.0, at.1))?;
        self.resolve(li, *src, *site, depth + 1)
    }

    /// Whether statement `(da, sa)` runs before `(b, s)` on every path to it.
    fn before(&self, da: usize, sa: usize, b: usize, s: usize) -> bool {
        if da == b {
            sa < s
        } else {
            self.doms.dominates(da, b)
        }
    }
}

/// Read `places` (with locals `locals`) in front of loop `lp`: every edge into its head from
/// outside goes to the reads first. Each place's pointers are tested for null, the root first;
/// a read not made leaves its local 0 (the loop does not read it then).
fn read_in_front(func: &mut Function, lp: &Loop, places: &[Place], locals: &[Local]) {
    // Every read place and pointer prefix: (place, local). Prefixes of the places that end
    // before a `Deref` are pointers, read like the places.
    let mut reads: Vec<(Place, Local)> =
        places.iter().cloned().zip(locals.iter().copied()).collect();
    for p in places {
        for (k, proj) in p.proj.iter().enumerate().skip(1) {
            if !matches!(proj, Proj::Deref(_)) {
                continue;
            }
            let prefix = Place {
                local: p.local,
                proj: p.proj[..k].to_vec(),
            };
            if !reads.iter().any(|(q, _)| *q == prefix) {
                let l = Local(func.locals.len() as u32);
                func.locals.push(LocalDecl::new(Ty::Ptr, None));
                reads.push((prefix, l));
            }
        }
    }
    // The pointers followed, shortest first: (place of the pointer, local holding it).
    let mut pointers: Vec<(Place, Local)> = vec![];
    for (p, _) in &reads {
        let k = last_deref(p);
        let ptr = Place {
            local: p.local,
            proj: p.proj[..k].to_vec(),
        };
        if !pointers.iter().any(|(q, _)| *q == ptr) {
            let l = if ptr.proj.is_empty() {
                ptr.local
            } else {
                reads
                    .iter()
                    .find(|(q, _)| *q == ptr)
                    .expect("ICE: pointer prefix is read")
                    .1
            };
            pointers.push((ptr, l));
        }
    }
    pointers.sort_by_key(|(p, _)| p.proj.len());
    let head = BlockId(lp.head as u32);
    // Build back to front: the last test continues into the head.
    let mut next = head;
    for (ptr, ptr_local) in pointers.iter().rev() {
        let mut stmts = vec![];
        for (p, l) in &reads {
            let k = last_deref(p);
            if p.proj[..k] == ptr.proj[..] && p.local == ptr.local {
                let from = Place {
                    local: *ptr_local,
                    proj: p.proj[k..].to_vec(),
                };
                stmts.push(Stmt::Assign(
                    Place::local(*l),
                    Rvalue::Use(Operand::Copy(from)),
                ));
            }
        }
        let read = add_block(func, stmts, Terminator::Goto(next));
        let nonnull = Local(func.locals.len() as u32);
        func.locals.push(LocalDecl::new(Ty::Bool, None));
        let test = Stmt::Assign(
            Place::local(nonnull),
            Rvalue::Binary(
                BinOp::Ne,
                Operand::Copy(Place::local(*ptr_local)),
                Operand::Const(Const::Int(0), Ty::Ptr),
            ),
        );
        next = add_block(
            func,
            vec![test],
            Terminator::Branch {
                cond: Operand::Copy(Place::local(nonnull)),
                then: read,
                els: next,
            },
        );
    }
    // Every local starts at 0, for the paths that skip its read.
    let zeros: Vec<Stmt> = reads
        .iter()
        .map(|(_, l)| {
            let ty = func.locals[l.0 as usize].ty;
            Stmt::Assign(Place::local(*l), Rvalue::Use(Operand::Const(zero(ty), ty)))
        })
        .collect();
    let first = add_block(func, zeros, Terminator::Goto(next));
    // The blocks added in front of loops come after `lp.body`'s; none of them jumps to its head.
    for bi in 0..lp.body.len() {
        if lp.body[bi] {
            continue;
        }
        successors_mut(&mut func.blocks[bi].term, &mut |t| {
            if *t == head {
                *t = first;
            }
        });
    }
}

/// Index of the last `Deref` in `p`'s projections (every hoisted place has one).
fn last_deref(p: &Place) -> usize {
    p.proj
        .iter()
        .rposition(|x| matches!(x, Proj::Deref(_)))
        .expect("ICE: hoisted read goes through a pointer")
}

fn zero(ty: Ty) -> Const {
    match ty {
        Ty::F32 | Ty::F64 => Const::Float(0.0),
        Ty::Bool => Const::Bool(false),
        _ => Const::Int(0),
    }
}

#[cfg(test)]
mod tests;
