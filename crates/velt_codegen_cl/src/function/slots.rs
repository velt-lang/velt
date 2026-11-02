//! Stack slots for memory locals, shared by locals that are never live at the same time.
//!
//! VIR names every temporary, and every aggregate local (a string, a union, an optional, a
//! struct) lives in memory, so a long function has thousands of them: a slot of its own for
//! each made frames of 100–400 KB (a server renderer's generator, #2048), and a recursion a few
//! dozen levels deep overflowed the stack in a debug build. LLVM splits most of them into
//! registers (SROA); Cranelift keeps every slot. So locals share slots here, the way a register
//! allocator shares registers:
//!
//! 1. **Live range.** A local's value matters from a write to a later read, so it is *live* at
//!    a point when some path reaches the point from an access (a write, possibly partial) and
//!    goes on to a read before any write of the whole local (strong liveness intersected with
//!    "maybe written", per block). Its *range* is the hull of the points, in block order, where
//!    it is live or accessed (every write, dead or not, writes the slot).
//! 2. **Pointers.** `p = &x` gives `p` (and what is computed from `p`: copies, offsets, casts,
//!    a `Ptr` returned by a call that received it) the *origin* `x`. Each use of such a pointer
//!    (a dereference, a call argument, a memory copy or fill) counts as a read of `x` there. A
//!    callee only borrows its pointer arguments for the call (calls borrow,
//!    docs/reference/memory), so the call is the last point it uses them. A pointer that
//!    leaves the function's registers (stored to memory, returned) makes `x` *escaped*: it
//!    keeps a slot of its own.
//! 3. **Assignment.** Locals are assigned in order of range start to the best-fitting slot whose
//!    locals' ranges have all ended (linear scan); a slot grows to its largest local.
//!
//! Locals that debug info describes keep a keyed slot of their own (debug_info finds them by
//! key), as do parameters and escaped locals.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

use velt_vir::vir::{self, Callee, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty};

use crate::abi::size_align;
use crate::CodegenResult;

/// Where each memory local goes: `local_slot[i]` indexes `slots` for a memory local `i`.
pub(super) struct SlotPlan {
    pub(super) local_slot: Vec<Option<u32>>,
    pub(super) slots: Vec<PlannedSlot>,
}

/// One stack slot: its size, alignment, and the local it is keyed by (debug info), if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlannedSlot {
    pub(super) size: u32,
    pub(super) align: u32,
    pub(super) key: Option<u32>,
}

/// How an access at a point uses a local's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Reads them (or may: a pointer use, an address taken).
    Read,
    /// Writes part of them.
    Partial,
    /// Writes all of them.
    Full,
}

/// Plan the slots of `function`'s memory locals; `keyed(i)` says whether local `i` needs a
/// keyed slot of its own.
pub(super) fn plan(
    program: &vir::Program,
    function: &vir::Function,
    in_memory: &[bool],
    keyed: &dyn Fn(usize) -> bool,
) -> CodegenResult<SlotPlan> {
    let size_align = |ty| size_align(program, ty);
    let n = function.locals.len();
    let mut local_slot = vec![None; n];
    let mut slots = Vec::new();
    // Candidates for sharing, by local.
    let mut shared = vec![false; n];
    for (i, local) in function.locals.iter().enumerate() {
        if !in_memory[i] || local.ty == Ty::Unit {
            continue;
        }
        let (size, align) = size_align(local.ty)?;
        if keyed(i) || i < function.params.len() {
            local_slot[i] = Some(slots.len() as u32);
            slots.push(PlannedSlot {
                size: size.max(1),
                align,
                key: Some(i as u32),
            });
        } else {
            shared[i] = true;
        }
    }
    if !shared.contains(&true) {
        return Ok(SlotPlan { local_slot, slots });
    }
    let scan = Scan::new(program, function, in_memory, &shared);
    let ranges = scan.ranges(function);

    // Escaped and unaccessed locals first: each escaped one alone, the unaccessed ones
    // (never named by the code) in one slot.
    let mut intervals = Vec::new();
    let mut unused = Vec::new();
    for i in 0..n {
        if !shared[i] {
            continue;
        }
        let (size, align) = size_align(function.locals[i].ty)?;
        let size = size.max(1);
        if scan.escaped[i] {
            local_slot[i] = Some(slots.len() as u32);
            slots.push(PlannedSlot {
                size,
                align,
                key: None,
            });
        } else {
            match ranges[i] {
                Some((start, end)) => intervals.push((start, end, i, size, align)),
                None => unused.push((i, size, align)),
            }
        }
    }
    intervals.sort_unstable_by_key(|&(start, _, i, _, _)| (start, i));
    // Linear scan: `active` holds (end, slot) of the slots in use; `free` the others by size.
    let mut active: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let mut free: BTreeSet<(u32, u32)> = BTreeSet::new();
    for (start, end, i, size, align) in intervals {
        while let Some(&Reverse((e, s))) = active.peek() {
            if e >= start {
                break;
            }
            active.pop();
            free.insert((slots[s as usize].size, s));
        }
        // The smallest free slot that fits, else the largest one (grown), else a new one.
        let pick = free
            .range((size, 0)..)
            .next()
            .or_else(|| free.iter().next_back())
            .copied();
        let s = match pick {
            Some(key) => {
                free.remove(&key);
                let slot = &mut slots[key.1 as usize];
                slot.size = slot.size.max(size);
                slot.align = slot.align.max(align);
                key.1
            }
            None => {
                slots.push(PlannedSlot {
                    size,
                    align,
                    key: None,
                });
                slots.len() as u32 - 1
            }
        };
        local_slot[i] = Some(s);
        active.push(Reverse((end, s)));
    }
    if !unused.is_empty() {
        let size = unused.iter().map(|u| u.1).max().unwrap_or(1);
        let align = unused.iter().map(|u| u.2).max().unwrap_or(1);
        let s = slots.len() as u32;
        slots.push(PlannedSlot {
            size,
            align,
            key: None,
        });
        for (i, _, _) in unused {
            local_slot[i] = Some(s);
        }
    }
    Ok(SlotPlan { local_slot, slots })
}

/// The pointer origins and the accesses of the shared locals.
struct Scan {
    /// Per local: the shared locals a value of it may point into (sorted).
    origins: Vec<Vec<u32>>,
    escaped: Vec<bool>,
    /// Per shared local: its accesses as (block, point, kind), in program order.
    accesses: Vec<Vec<(u32, u32, Kind)>>,
    /// Per block: its first point (block start) and its last (block end).
    bounds: Vec<(u32, u32)>,
}

/// The value-producing parts of a statement or terminator, for the origin analysis.
struct Ctx<'a> {
    program: &'a vir::Program,
    in_memory: &'a [bool],
    shared: &'a [bool],
    locals: &'a [vir::LocalDecl],
}

impl Ctx<'_> {
    fn is_memory(&self, local: u32) -> bool {
        self.in_memory.get(local as usize).copied().unwrap_or(false)
    }

    /// The declared result of a call of `callee` (`None` for an unknown one).
    fn ret(&self, callee: &Callee) -> Option<Ty> {
        match callee {
            Callee::Func(id) => self.program.funcs.get(id.0 as usize).map(|f| f.ret),
            Callee::Extern(id) => self.program.externs.get(id.0 as usize).map(|e| e.ret),
            Callee::Ptr { ret, .. } => Some(*ret),
        }
    }

    /// Whether a register local of this type can carry a pointer.
    fn carries(&self, local: u32) -> bool {
        matches!(
            self.locals.get(local as usize).map(|l| l.ty),
            Some(Ty::Ptr | Ty::I64 | Ty::U64)
        )
    }
}

impl Scan {
    fn new(
        program: &vir::Program,
        function: &vir::Function,
        in_memory: &[bool],
        shared: &[bool],
    ) -> Scan {
        let n = function.locals.len();
        let ctx = Ctx {
            program,
            in_memory,
            shared,
            locals: &function.locals,
        };
        let mut scan = Scan {
            origins: vec![Vec::new(); n],
            escaped: vec![false; n],
            accesses: vec![Vec::new(); n],
            bounds: Vec::with_capacity(function.blocks.len()),
        };
        // Origins and escapes: a flow-insensitive fixpoint (both only grow).
        loop {
            let mut changed = false;
            for block in &function.blocks {
                for s in &block.stmts {
                    changed |= scan.flow_stmt(&ctx, s);
                }
                changed |= scan.flow_term(&ctx, &block.term);
            }
            if !changed {
                break;
            }
        }
        // Accesses, in program order.
        let mut point = 0u32;
        for (bi, block) in function.blocks.iter().enumerate() {
            let start = point;
            point += 1;
            let mut events = Vec::new();
            for s in &block.stmts {
                stmt_events(&ctx, &scan.origins, s, &mut events);
                scan.record(bi as u32, point, &mut events);
                point += 1;
            }
            term_events(&ctx, &scan.origins, &block.term, &mut events);
            scan.record(bi as u32, point, &mut events);
            point += 1;
            scan.bounds.push((start, point));
            point += 1;
        }
        scan
    }

    fn record(&mut self, block: u32, point: u32, events: &mut Vec<(u32, Kind)>) {
        for (local, kind) in events.drain(..) {
            if let Some(a) = self.accesses.get_mut(local as usize) {
                a.push((block, point, kind));
            }
        }
    }

    /// The origins of `op`'s value.
    fn value_origins(&self, ctx: &Ctx, op: &Operand, out: &mut Vec<u32>) {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() && !ctx.is_memory(p.local.0) {
                out.extend_from_slice(&self.origins[p.local.0 as usize]);
            }
        }
    }

    /// The origins of `&place`.
    fn addr_origins(&self, ctx: &Ctx, place: &Place, out: &mut Vec<u32>) {
        let local = place.local.0;
        if ctx.is_memory(local) {
            if !place.proj.iter().any(|p| matches!(p, Proj::Deref(_)))
                && ctx.shared.get(local as usize).copied().unwrap_or(false)
            {
                out.push(local);
            }
        } else if matches!(place.proj.first(), Some(Proj::Deref(_))) {
            out.extend_from_slice(&self.origins[local as usize]);
        }
    }

    fn rvalue_origins(&self, ctx: &Ctx, rv: &Rvalue, out: &mut Vec<u32>) {
        match rv {
            Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => {
                self.value_origins(ctx, a, out)
            }
            Rvalue::Binary(_, a, b) => {
                self.value_origins(ctx, a, out);
                self.value_origins(ctx, b, out);
            }
            Rvalue::AddrOf(p) => self.addr_origins(ctx, p, out),
            Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => {
                for o in ops {
                    self.value_origins(ctx, o, out);
                }
            }
        }
    }

    /// A value with `origins` goes to `dest`: a register local that can hold a pointer gets
    /// them, any other destination makes them escape. Returns whether anything changed.
    fn flow_into(&mut self, ctx: &Ctx, dest: &Place, origins: &mut Vec<u32>) -> bool {
        if origins.is_empty() {
            return false;
        }
        origins.sort_unstable();
        origins.dedup();
        let local = dest.local.0;
        if dest.proj.is_empty() && !ctx.is_memory(local) {
            if !ctx.carries(local) {
                return false;
            }
            let have = &mut self.origins[local as usize];
            let before = have.len();
            have.extend_from_slice(origins);
            have.sort_unstable();
            have.dedup();
            have.len() != before
        } else {
            self.escape(origins)
        }
    }

    fn escape(&mut self, origins: &[u32]) -> bool {
        let mut changed = false;
        for &o in origins {
            if let Some(e) = self.escaped.get_mut(o as usize) {
                changed |= !*e;
                *e = true;
            }
        }
        changed
    }

    fn flow_stmt(&mut self, ctx: &Ctx, s: &Stmt) -> bool {
        let Stmt::Assign(dest, rv) = s else {
            return false;
        };
        let mut origins = Vec::new();
        self.rvalue_origins(ctx, rv, &mut origins);
        self.flow_into(ctx, dest, &mut origins)
    }

    fn flow_term(&mut self, ctx: &Ctx, t: &Terminator) -> bool {
        match t {
            Terminator::Call {
                callee, args, dest, ..
            } => {
                // Only a pointer result can carry an argument's address back.
                let Some(dest) = dest else {
                    return false;
                };
                if ctx.ret(callee) != Some(Ty::Ptr) {
                    return false;
                }
                let mut origins = Vec::new();
                for a in args {
                    self.value_origins(ctx, a, &mut origins);
                }
                self.flow_into(ctx, dest, &mut origins)
            }
            Terminator::Return(op) => {
                let mut origins = Vec::new();
                self.value_origins(ctx, op, &mut origins);
                self.escape(&origins)
            }
            _ => false,
        }
    }
}

/// The accesses `place` makes: to its local's bytes when the local is in memory (`kind` for
/// a destination, a read otherwise), through the pointer it dereferences otherwise.
fn place_events(
    ctx: &Ctx,
    origins: &[Vec<u32>],
    place: &Place,
    kind: Kind,
    out: &mut Vec<(u32, Kind)>,
) {
    let local = place.local.0;
    if ctx.is_memory(local) {
        let derefs = place.proj.iter().any(|p| matches!(p, Proj::Deref(_)));
        let kind = match kind {
            // Writing through a pointer stored in the local only reads the local.
            _ if derefs => Kind::Read,
            Kind::Full if !place.proj.is_empty() => Kind::Partial,
            k => k,
        };
        out.push((local, kind));
    } else if matches!(place.proj.first(), Some(Proj::Deref(_))) {
        pointer_use(origins, local, out);
    }
}

/// A use of register local `local` as a pointer: a read of everything it may point into.
fn pointer_use(origins: &[Vec<u32>], local: u32, out: &mut Vec<(u32, Kind)>) {
    if let Some(o) = origins.get(local as usize) {
        out.extend(o.iter().map(|&x| (x, Kind::Read)));
    }
}

fn operand_events(ctx: &Ctx, origins: &[Vec<u32>], op: &Operand, out: &mut Vec<(u32, Kind)>) {
    if let Operand::Copy(p) = op {
        place_events(ctx, origins, p, Kind::Read, out);
    }
}

/// A pointer operand handed to code that may read or write through it.
fn pointer_operand(ctx: &Ctx, origins: &[Vec<u32>], op: &Operand, out: &mut Vec<(u32, Kind)>) {
    operand_events(ctx, origins, op, out);
    if let Operand::Copy(p) = op {
        if p.proj.is_empty() && !ctx.is_memory(p.local.0) {
            pointer_use(origins, p.local.0, out);
        }
    }
}

/// A statement's accesses: reads (and pointer uses) first, then the write.
fn stmt_events(ctx: &Ctx, origins: &[Vec<u32>], s: &Stmt, out: &mut Vec<(u32, Kind)>) {
    match s {
        Stmt::Assign(dest, rv) => {
            match rv {
                Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => {
                    operand_events(ctx, origins, a, out)
                }
                Rvalue::Binary(_, a, b) => {
                    operand_events(ctx, origins, a, out);
                    operand_events(ctx, origins, b, out);
                }
                Rvalue::AddrOf(p) => {
                    // Taking the address reads a pointer the place goes through, and counts
                    // as an access of a memory local (its range then covers the pointer).
                    let local = p.local.0;
                    if ctx.is_memory(local) {
                        out.push((local, Kind::Read));
                    }
                }
                Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => {
                    for o in ops {
                        operand_events(ctx, origins, o, out);
                    }
                }
            }
            // An aggregate of another layout than the local's writes only its own fields.
            let whole = match rv {
                Rvalue::Aggregate(id, _) => ctx
                    .locals
                    .get(dest.local.0 as usize)
                    .is_some_and(|l| l.ty == Ty::Agg(*id)),
                _ => true,
            };
            let kind = if whole { Kind::Full } else { Kind::Partial };
            place_events(ctx, origins, dest, kind, out);
        }
        Stmt::MemCopy { dst, src, .. } => {
            pointer_operand(ctx, origins, src, out);
            pointer_operand(ctx, origins, dst, out);
        }
        Stmt::MemCopyDyn { dst, src, len, .. } => {
            pointer_operand(ctx, origins, src, out);
            operand_events(ctx, origins, len, out);
            pointer_operand(ctx, origins, dst, out);
        }
        Stmt::MemSet { dst, byte, len } => {
            operand_events(ctx, origins, byte, out);
            operand_events(ctx, origins, len, out);
            pointer_operand(ctx, origins, dst, out);
        }
        Stmt::Nop => {}
    }
}

fn term_events(ctx: &Ctx, origins: &[Vec<u32>], t: &Terminator, out: &mut Vec<(u32, Kind)>) {
    match t {
        Terminator::Branch { cond: op, .. }
        | Terminator::Switch { value: op, .. }
        | Terminator::Return(op) => pointer_operand(ctx, origins, op, out),
        Terminator::Call {
            callee,
            args,
            dest,
            ..
        } => {
            if let Callee::Ptr { target, .. } = callee {
                pointer_operand(ctx, origins, target, out);
            }
            for a in args {
                pointer_operand(ctx, origins, a, out);
            }
            if let Some(dest) = dest {
                place_events(ctx, origins, dest, Kind::Full, out);
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn successors(t: &Terminator) -> Vec<u32> {
    match t {
        Terminator::Goto(b) => vec![b.0],
        Terminator::Branch { then, els, .. } => vec![then.0, els.0],
        Terminator::Switch { cases, default, .. } => {
            let mut v: Vec<u32> = cases.iter().map(|(_, b)| b.0).collect();
            v.push(default.0);
            v
        }
        Terminator::Call { next, .. } => vec![next.0],
        Terminator::Return(_) | Terminator::Unreachable => vec![],
    }
}

impl Scan {
    /// Per local: the hull (first, last point) of where it is accessed or live, or `None`
    /// when it is never accessed.
    fn ranges(&self, function: &vir::Function) -> Vec<Option<(u32, u32)>> {
        let nb = function.blocks.len();
        let succs: Vec<Vec<u32>> = function
            .blocks
            .iter()
            .map(|b| {
                let mut s = successors(&b.term);
                s.retain(|&x| (x as usize) < nb);
                s
            })
            .collect();
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); nb];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s as usize].push(b as u32);
            }
        }
        // Per-block marks, stamped with the local being processed (no clearing between locals).
        let mut live_in = vec![u32::MAX; nb];
        let mut live_out = vec![u32::MAX; nb];
        let mut init_in = vec![u32::MAX; nb];
        let mut touched = vec![u32::MAX; nb];
        let mut kills = vec![u32::MAX; nb];
        let mut work: Vec<u32> = Vec::new();
        let mut live_blocks: Vec<u32> = Vec::new();
        let mut ranges = vec![None; self.accesses.len()];
        for (local, acc) in self.accesses.iter().enumerate() {
            if acc.is_empty() {
                continue;
            }
            let stamp = local as u32;
            let mut lo = u32::MAX;
            let mut hi = 0u32;
            // Upward-exposed reads (a read before any whole write in its block) and kills.
            work.clear();
            live_blocks.clear();
            let mut prev_block = u32::MAX;
            let mut killed = false;
            for &(b, p, kind) in acc {
                lo = lo.min(p);
                hi = hi.max(p);
                if b != prev_block {
                    prev_block = b;
                    killed = false;
                    touched[b as usize] = stamp;
                }
                match kind {
                    Kind::Read if !killed && live_in[b as usize] != stamp => {
                        live_in[b as usize] = stamp;
                        live_blocks.push(b);
                        work.push(b);
                    }
                    Kind::Full => {
                        killed = true;
                        kills[b as usize] = stamp;
                    }
                    _ => {}
                }
            }
            // Backward liveness from the upward-exposed reads.
            while let Some(b) = work.pop() {
                for &p in &preds[b as usize] {
                    live_out[p as usize] = stamp;
                    if kills[p as usize] != stamp && live_in[p as usize] != stamp {
                        live_in[p as usize] = stamp;
                        live_blocks.push(p);
                        work.push(p);
                    }
                }
            }
            // "Maybe written" within the live blocks: forward from the accessing blocks.
            work.clear();
            for &(b, _, _) in acc {
                if work.last() != Some(&b) {
                    work.push(b);
                }
            }
            while let Some(b) = work.pop() {
                for &s in &succs[b as usize] {
                    if live_in[s as usize] == stamp && init_in[s as usize] != stamp {
                        init_in[s as usize] = stamp;
                        work.push(s);
                    }
                }
            }
            for &b in &live_blocks {
                let bi = b as usize;
                if init_in[bi] == stamp {
                    lo = lo.min(self.bounds[bi].0);
                }
            }
            // Live out and written: the block's end. A block is live-out only as a
            // predecessor of a live-in block, so look at those.
            for &b in &live_blocks {
                for &p in &preds[b as usize] {
                    let pi = p as usize;
                    if live_out[pi] == stamp && (init_in[pi] == stamp || touched[pi] == stamp) {
                        hi = hi.max(self.bounds[pi].1);
                    }
                }
            }
            ranges[local] = Some((lo, hi));
        }
        ranges
    }
}
