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
//!    "maybe written", per block). Its *range* is the hull of the points, in reverse
//!    postorder, where it is live or accessed (every write, dead or not, writes the slot).
//! 2. **Pointers.** `p = &x` gives `p` (and what is computed from `p`: copies, offsets, casts,
//!    a `Ptr` returned by a call that received it) the *origin* `x`. Taking the address is an
//!    access of `x` (its range covers it) but reads nothing; each use of such a pointer (a
//!    dereference, a call argument, a memory copy or fill) counts as a read of `x` there. A
//!    callee only borrows its pointer arguments for the call (calls borrow,
//!    https://veltlang.com/docs/reference/memory/), and carries an argument's address out only
//!    through a `Ptr` result (docs/internals/contracts/README.md "Pointer arguments"), so the
//!    call is the last point it uses them. A pointer that leaves the function's registers
//!    (stored to memory, returned) makes `x` *escaped*: it keeps a slot of its own. The
//!    exception is a function's out-pointer (its last param, which lowering names `ret.out`):
//!    the callee writes its result there without reading what was there, so a call that passes
//!    exactly `&x` for it writes all of `x`.
//! 3. **Assignment.** Locals are assigned in order of range start to the best-fitting slot whose
//!    locals' ranges have all ended (linear scan); a slot grows to its largest local.
//!
//! Locals that debug info describes keep a keyed slot of their own (debug_info finds them by
//! key), as do parameters; escaped locals keep a slot of their own without a key.
//!
//! The liveness walk of a local only visits blocks some access of it can reach: where no write
//! can have happened, a read has no value to keep (the intersection above drops those blocks
//! anyway). With the out-pointer writes, a temporary's walk stops at the call that writes it,
//! so the work grows with the function's length, not with its square (one walk back to the
//! entry per temporary). A walk that still visits more than `WALK_BUDGET` blocks (a value used
//! all over a long loop) stops, and its local keeps a slot of its own.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

use velt_vir::vir::{self, Callee, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty};

use crate::abi::size_align;
use crate::CodegenResult;

mod cfg;
use cfg::Cfg;

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
    /// Reads them (or may: a pointer use).
    Read,
    /// Takes their address: an access, but the bytes are read where the pointer is used.
    Addr,
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
    plan_counted(program, function, in_memory, keyed).map(|(plan, _)| plan)
}

/// [`plan`], and the number of block visits its liveness walks made (for tests of its cost).
pub(super) fn plan_counted(
    program: &vir::Program,
    function: &vir::Function,
    in_memory: &[bool],
    keyed: &dyn Fn(usize) -> bool,
) -> CodegenResult<(SlotPlan, u64)> {
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
        return Ok((SlotPlan { local_slot, slots }, 0));
    }
    let scan = Scan::new(program, function, in_memory, &shared);
    let (ranges, visits) = scan.ranges();

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
    Ok((SlotPlan { local_slot, slots }, visits))
}

/// The pointer origins and the accesses of the shared locals.
struct Scan {
    /// Per local: the shared locals a value of it may point into (sorted).
    origins: Vec<Vec<u32>>,
    escaped: Vec<bool>,
    /// Per register local: `Some(x)` when every value it is given is `&x` for a shared local
    /// `x` (the whole local), so it points to exactly `x`.
    addr_of: Vec<Option<u32>>,
    /// Per shared local: its accesses as (block, point, kind), in program order.
    accesses: Vec<Vec<(u32, u32, Kind)>>,
    /// Per block: its first point (block start) and its last (block end). Points follow the
    /// blocks in reverse postorder, the order control reaches them, so a range is a short
    /// hull even where blocks are laid out apart from where they run (inlined code).
    bounds: Vec<(u32, u32)>,
    cfg: Cfg,
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

    /// Whether a call of `callee` with `nargs` arguments passes its last one as the callee's
    /// out-pointer: the last param of a function, which lowering names `ret.out` and the
    /// callee writes before it reads it (vir.rs invariant 2).
    fn out_call(&self, callee: &Callee, nargs: usize) -> bool {
        let Callee::Func(id) = callee else {
            return false;
        };
        let Some(f) = self.program.funcs.get(id.0 as usize) else {
            return false;
        };
        nargs > 0
            && nargs == f.params.len()
            && f.ret == Ty::Unit
            && f.params[nargs - 1] == Ty::Ptr
            && f.locals.get(nargs - 1).and_then(|l| l.name.as_deref()) == Some("ret.out")
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
            addr_of: addr_of(&ctx, function),
            accesses: vec![Vec::new(); n],
            bounds: vec![(0, 0); function.blocks.len()],
            cfg: Cfg::new(function),
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
        for i in 0..scan.cfg.order.len() {
            let bi = scan.cfg.order[i] as usize;
            let block = &function.blocks[bi];
            let start = point;
            point += 1;
            let mut events = Vec::new();
            for s in &block.stmts {
                stmt_events(&ctx, &scan.origins, s, &mut events);
                scan.record(bi as u32, point, &mut events);
                point += 1;
            }
            term_events(&ctx, &scan.origins, &scan.addr_of, &block.term, &mut events);
            scan.record(bi as u32, point, &mut events);
            point += 1;
            scan.bounds[bi] = (start, point);
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

/// Per register local: the shared local `x` when its only values are `&x` (`Scan::addr_of`).
fn addr_of(ctx: &Ctx, function: &vir::Function) -> Vec<Option<u32>> {
    // Per local: `None` before any value, then `Some(Some(x))` while every value was `&x`.
    let mut seen: Vec<Option<Option<u32>>> = vec![None; function.locals.len()];
    let mut give = |dest: &Place, value: Option<u32>| {
        let value = if dest.proj.is_empty() { value } else { None };
        if let Some(s) = seen.get_mut(dest.local.0 as usize) {
            *s = match *s {
                None => Some(value),
                Some(v) if v == value => Some(v),
                Some(_) => Some(None),
            };
        }
    };
    for block in &function.blocks {
        for s in &block.stmts {
            if let Stmt::Assign(dest, rv) = s {
                let x = match rv {
                    Rvalue::AddrOf(p)
                        if p.proj.is_empty()
                            && ctx.is_memory(p.local.0)
                            && ctx.shared.get(p.local.0 as usize).copied().unwrap_or(false) =>
                    {
                        Some(p.local.0)
                    }
                    _ => None,
                };
                give(dest, x);
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            give(d, None);
        }
    }
    seen.into_iter()
        .enumerate()
        .map(|(i, s)| match s {
            Some(Some(x)) if !ctx.is_memory(i as u32) => Some(x),
            _ => None,
        })
        .collect()
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
                    // Taking the address of a memory local is an access (its range then
                    // covers the pointer) that reads nothing, unless the place goes through
                    // a pointer the local holds.
                    let local = p.local.0;
                    if ctx.is_memory(local) {
                        let derefs = p.proj.iter().any(|q| matches!(q, Proj::Deref(_)));
                        out.push((local, if derefs { Kind::Read } else { Kind::Addr }));
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

fn term_events(
    ctx: &Ctx,
    origins: &[Vec<u32>],
    addr_of: &[Option<u32>],
    t: &Terminator,
    out: &mut Vec<(u32, Kind)>,
) {
    match t {
        Terminator::Branch { cond: op, .. }
        | Terminator::Switch { value: op, .. }
        | Terminator::Return(op) => pointer_operand(ctx, origins, op, out),
        Terminator::Call {
            callee, args, dest, ..
        } => {
            if let Callee::Ptr { target, .. } = callee {
                pointer_operand(ctx, origins, target, out);
            }
            let out_call = ctx.out_call(callee, args.len());
            for (i, a) in args.iter().enumerate() {
                // Exactly `&x` as the callee's out-pointer: the call writes all of `x`.
                let whole = match a {
                    Operand::Copy(p) if out_call && i + 1 == args.len() && p.proj.is_empty() => {
                        addr_of.get(p.local.0 as usize).copied().flatten()
                    }
                    _ => None,
                };
                match whole {
                    Some(x) => out.push((x, Kind::Full)),
                    None => pointer_operand(ctx, origins, a, out),
                }
            }
            if let Some(dest) = dest {
                place_events(ctx, origins, dest, Kind::Full, out);
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

/// The most blocks one local's liveness walk visits. A local live in more (all over a long
/// loop) keeps a slot of its own, so the planner's work stays linear in the function's length.
pub(super) const WALK_BUDGET: u32 = 1024;

impl Scan {
    /// Per local: the hull (first, last point) of where it is accessed or live, or `None`
    /// when it is never accessed.
    fn ranges(&self) -> (Vec<Option<(u32, u32)>>, u64) {
        let nb = self.bounds.len();
        let Cfg {
            succs,
            preds,
            num,
            reached_from,
            ..
        } = &self.cfg;
        let mut visits = 0u64;
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
            // A block can have been written in (entered after an access) only if some access
            // block reaches it: `reached_from[b] >= first`.
            let first = acc.iter().map(|a| num[a.0 as usize]).min().unwrap_or(0);
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
                    Kind::Read
                        if !killed
                            && live_in[b as usize] != stamp
                            && reached_from[b as usize] >= first =>
                    {
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
            let mut budget = WALK_BUDGET;
            while let Some(b) = work.pop() {
                visits += 1;
                budget -= 1;
                if budget == 0 {
                    break;
                }
                for &p in &preds[b as usize] {
                    live_out[p as usize] = stamp;
                    if kills[p as usize] != stamp
                        && live_in[p as usize] != stamp
                        && reached_from[p as usize] >= first
                    {
                        live_in[p as usize] = stamp;
                        live_blocks.push(p);
                        work.push(p);
                    }
                }
            }
            if budget == 0 {
                // Too long a walk (a value used all over a long loop): the whole function is
                // a range that holds, and leaves the local a slot of its own.
                ranges[local] = Some((0, u32::MAX));
                continue;
            }
            // "Maybe written" within the live blocks: forward from the accessing blocks.
            work.clear();
            for &(b, _, _) in acc {
                if work.last() != Some(&b) {
                    work.push(b);
                }
            }
            while let Some(b) = work.pop() {
                visits += 1;
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
        (ranges, visits)
    }
}
