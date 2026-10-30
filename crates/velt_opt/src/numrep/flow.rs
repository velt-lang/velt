//! Facts (`fact.rs`) for the tracked locals at every block entry: abstract interpretation over
//! the CFG, refined on branch conditions and widened to thresholds at loop heads.
//!
//! - **Refinement**: on the edges of `a < b` (and `<=`, `>`, `>=`, `==`, `!=`, over doubles or
//!   integers) both operands are narrowed, and so is the integer an operand was converted from
//!   in the same block (`(i as f64) < n` bounds `i`). A false float comparison refines nothing
//!   when an operand may be NaN.
//! - **Widening**: a loop head's entry is widened after `WIDEN_AFTER` arrivals. A bound that
//!   still moves jumps to the next of `THRESHOLDS`, so each bound moves a handful of times.
//!   Induction variables need nothing more: only the moving bound widens, and the exit test
//!   bounds it on the loop's edges.
//! - **Sparse states**: a block's entry state holds the tracked locals live into it only, so a
//!   visit costs the block's statements and the locals live across its edges, not every tracked
//!   local: a function made big by inlining stays linear (#1658). Code reading the states
//!   (`load`) reads the locals its block uses, which are live there or set before.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use velt_vir::vir::{
    AggId, BinOp, Callee, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty, UnOp,
};

use super::fact::{self, Fact, TWO_53};
use super::Env;

/// Arrivals at a loop head before its entry state is widened.
const WIDEN_AFTER: u32 = 2;
/// Largest tracked-locals × blocks product analysed (bounds memory and time). A block keeps
/// the facts of the locals live into it only, so the work grows with the function's length,
/// not with this product: the bound is for the per-block bookkeeping. A `main` that inlines a
/// few loops over typed arrays has about 700 × 900 (`bench/typed_array/loops.ts`): past the
/// bound, none of its loop counters would be integers.
const MAX_CELLS: usize = 1 << 21;
/// Bounds a widened interval jumps to: 0, the int32/uint32 limits, 2^32 (one past a string's
/// largest length: an index that steps past the end), ±2^53 and ±∞.
const THRESHOLDS: [f64; 9] = [
    f64::NEG_INFINITY,
    -TWO_53,
    -2_147_483_648.0,
    0.0,
    2_147_483_647.0,
    4_294_967_295.0,
    4_294_967_296.0,
    TWO_53,
    f64::INFINITY,
];

/// `s.charCodeAt(i)` past the inline ASCII path (`velt_rt_str_char_code_at`).
const CHAR_CODE_AT: &str = "velt_rt_str_char_code_at";

/// The largest array length `numrep` assumes: 2^53 - 3. Arrays are enforced to hold at most
/// `velt_vir::MAX_ARRAY_LEN` (2^52 - 1) elements wherever a buffer is sized, so this holds; it is
/// just low enough that an index running to the length inclusive (`i <= xs.length`,
/// `i < xs.length + 1`) stays below 2^53 with room for its next step, and no lower, so the sum
/// of two indexes may still pass 2^53 and keeps the representation it had (a tighter bound
/// would narrow `i + j` used only as a double, which adds conversions: spectral-norm).
const ARRAY_LEN_MAX: i128 = (1 << 53) - 3;
const _: () = assert!(velt_vir::MAX_ARRAY_LEN as i128 <= ARRAY_LEN_MAX);

/// A fact per tracked local. One loaded for a block (`Flow::load`) holds the facts of the locals
/// live into the block; the others are stale, and nothing reads them before setting them.
pub(super) type State = Vec<Fact>;

/// Block entry facts of one function.
pub(super) struct Flow {
    /// Local → index into a state, for tracked locals.
    slot: Vec<Option<usize>>,
    /// Type per tracked local.
    tys: Vec<Ty>,
    /// Per block: the tracked locals live into it (state indexes, ascending).
    live_in: Vec<Vec<u32>>,
    /// Per block: the facts of `live_in` at its entry (`None` if unreachable).
    entry: Vec<Option<Vec<Fact>>>,
    /// A state of `Fact::top` per tracked local.
    tops: State,
    /// Facts joined, loaded, refined and transferred while solving (a cost measure, #1658).
    work: u64,
    /// The arrays' aggregate (`Env::array`).
    array: Option<AggId>,
    /// Per aggregate, its field types (`Env::field_tys`).
    field_tys: std::rc::Rc<[Vec<Ty>]>,
    /// Per tracked local: facts that hold for every value it takes (`counter`), met with its
    /// facts wherever it changes.
    caps: Vec<Option<Fact>>,
    /// Predecessors of each block.
    preds: Vec<Vec<usize>>,
    /// Per local: facts about the array elements it points at (`arrays`).
    elems: Vec<Option<Fact>>,
}

impl Flow {
    /// Analyse `func`, tracking `tracked`, with `elems` the facts about the array elements each
    /// pointer local reaches (`arrays`; may be empty). `None` when the function is too big.
    pub fn compute(
        func: &Function,
        env: &Env,
        tracked: &[Local],
        elems: Vec<Option<Fact>>,
    ) -> Option<Flow> {
        if tracked.len().saturating_mul(func.blocks.len()) > MAX_CELLS {
            return None;
        }
        let mut slot = vec![None; func.locals.len()];
        for (i, l) in tracked.iter().enumerate() {
            slot[l.0 as usize] = Some(i);
        }
        let tys: Vec<Ty> = tracked
            .iter()
            .map(|l| func.locals[l.0 as usize].ty)
            .collect();
        let live_in = super::liveness::live_in(func, &slot, tracked.len());
        let tops = tys.iter().map(|t| Fact::top(*t)).collect();
        let mut flow = Flow {
            slot,
            tys,
            live_in,
            entry: vec![None; func.blocks.len()],
            tops,
            work: 0,
            array: env.array,
            field_tys: env.field_tys.clone(),
            caps: vec![None; tracked.len()],
            preds: crate::map_probe::region::predecessors(func),
            elems,
        };
        flow.solve(func, env);
        // Counters are bounded by the trip counts the first solution proves.
        let caps = super::counter::caps(func, &flow);
        if caps.iter().any(Option::is_some) {
            flow.caps = caps;
            flow.entry = vec![None; func.blocks.len()];
            flow.solve(func, env);
        }
        Some(flow)
    }

    /// The number of tracked locals.
    pub fn slots(&self) -> usize {
        self.tys.len()
    }

    /// The index of `l` in a state, if tracked.
    pub fn slot(&self, l: Local) -> Option<usize> {
        self.slot.get(l.0 as usize).copied().flatten()
    }

    /// Facts about `l`, read by statement `si` of block `b`, before it (`None` if unreachable
    /// or untracked).
    pub fn fact_before(&self, func: &Function, b: usize, si: usize, l: Local) -> Option<Fact> {
        let i = self.slot(l)?;
        let mut st = self.tops.clone();
        if !self.load(b, &mut st) {
            return None;
        }
        for s in &func.blocks[b].stmts[..si] {
            self.transfer(&mut st, func, s);
        }
        Some(st[i])
    }

    /// A state for [`Flow::load`] to fill.
    pub fn scratch(&self) -> State {
        self.tops.clone()
    }

    /// Set the locals live into block `b` in `st` to their facts at its entry; false (and `st`
    /// unchanged) if `b` is unreachable. The other locals keep what `st` held: a statement
    /// reads only locals live before it, which are live into the block or set in it.
    pub fn load(&self, b: usize, st: &mut State) -> bool {
        let Some(e) = &self.entry[b] else {
            return false;
        };
        for (&i, f) in self.live_in[b].iter().zip(e) {
            st[i as usize] = *f;
        }
        true
    }

    /// The facts joined, loaded, refined and transferred while solving.
    #[cfg(test)]
    pub fn work(&self) -> u64 {
        self.work
    }

    /// `f` met with the cap of tracked local `i`.
    fn capped(&self, i: usize, f: Fact) -> Fact {
        match self.caps[i] {
            Some(c) if !f.is_empty() => meet(f, c),
            _ => f,
        }
    }

    /// Facts about the numeric operand `op` in `st` (`None` for other types).
    pub fn operand(&self, st: &State, func: &Function, op: &Operand) -> Option<Fact> {
        match op {
            Operand::Const(c, ty) => Fact::of_const(c, *ty),
            Operand::Copy(p) if p.proj.is_empty() => {
                let ty = func.locals[p.local.0 as usize].ty;
                if !(ty.is_int() || ty.is_float() || ty == Ty::Bool) {
                    return None;
                }
                Some(match self.slot[p.local.0 as usize] {
                    Some(i) => st[i],
                    None => Fact::top(ty),
                })
            }
            Operand::Copy(p) if self.is_array_len(func, p) => Some(Fact::int(0, ARRAY_LEN_MAX)),
            Operand::Copy(p)
                if matches!(p.proj[..], [Proj::Deref(_)])
                    && self
                        .elems
                        .get(p.local.0 as usize)
                        .copied()
                        .flatten()
                        .is_some() =>
            {
                self.elems[p.local.0 as usize]
            }
            Operand::Copy(p) => {
                let ty = crate::visit::derefs(p).then(|| match p.proj.last() {
                    Some(velt_vir::vir::Proj::Deref(t)) => *t,
                    _ => Ty::Unit,
                })?;
                (ty.is_int() || ty.is_float()).then(|| Fact::top(ty))
            }
        }
    }

    /// The type of the value at place `p`, if its projections are well-formed.
    fn place_ty(&self, func: &Function, p: &Place) -> Option<Ty> {
        let mut ty = func.locals.get(p.local.0 as usize)?.ty;
        for proj in &p.proj {
            ty = match (proj, ty) {
                (Proj::Deref(t), Ty::Ptr) => *t,
                (Proj::Cast(id), _) => Ty::Agg(*id),
                (Proj::Field(n), Ty::Agg(id)) => {
                    *self.field_tys.get(id.0 as usize)?.get(*n as usize)?
                }
                _ => return None,
            };
        }
        Some(ty)
    }

    /// Is `p` the length of an array (field 1 of the arrays' aggregate)?
    fn is_array_len(&self, func: &Function, p: &Place) -> bool {
        let Some(array) = self.array else {
            return false;
        };
        let [ref base @ .., Proj::Field(1)] = p.proj[..] else {
            return false;
        };
        let ty = match base.last() {
            None => func.locals[p.local.0 as usize].ty,
            Some(Proj::Deref(t)) => *t,
            Some(Proj::Cast(id)) => Ty::Agg(*id),
            Some(_) => return false,
        };
        ty == Ty::Agg(array)
    }

    /// Facts about the value of `rv`, assigned to a local of type `ty`.
    pub fn rvalue(&self, st: &State, func: &Function, rv: &Rvalue, ty: Ty) -> Fact {
        // A field read (`len & mask`) is any value of the field's type: its fact and type
        // together, here only (other users of `operand` and `operand_ty` see no fact for it).
        let field = |op: &Operand| match op {
            Operand::Copy(p) if !p.proj.is_empty() && operand_ty(func, op) == Ty::Unit => {
                self.place_ty(func, p).filter(|t| t.is_int())
            }
            _ => None,
        };
        let arg = |op: &Operand| {
            self.operand(st, func, op)
                .or_else(|| field(op).map(Fact::top))
        };
        let optype = |op: &Operand| field(op).unwrap_or_else(|| operand_ty(func, op));
        let f = match rv {
            Rvalue::Use(op) => arg(op),
            Rvalue::Cast(op, to) => arg(op).map(|a| fact::cast(optype(op), *to, a)),
            Rvalue::Unary(op @ UnOp::Neg, a) => arg(a).map(|x| fact::unary(*op, optype(a), x)),
            Rvalue::Binary(op, a, b) => match (arg(a), arg(b)) {
                (Some(x), Some(y)) => Some(fact::binary(*op, optype(a), x, y)),
                _ => None,
            },
            _ => None,
        };
        f.unwrap_or_else(|| Fact::top(ty))
    }

    /// Apply statement `s` to `st`.
    pub fn transfer(&self, st: &mut State, func: &Function, s: &Stmt) {
        let Stmt::Assign(dst, rv) = s else { return };
        if !dst.proj.is_empty() {
            return;
        }
        let Some(i) = self.slot[dst.local.0 as usize] else {
            return;
        };
        st[i] = self.capped(i, self.rvalue(st, func, rv, self.tys[i]));
    }

    /// Facts about the result of a call ending a block in state `st`.
    pub fn call_result(&self, st: &State, func: &Function, env: &Env, t: &Terminator) -> Fact {
        let Terminator::Call {
            callee,
            args,
            dest: Some(d),
            ..
        } = t
        else {
            return Fact::top(Ty::Unit);
        };
        let ty = func.locals[d.local.0 as usize].ty;
        let arg = args.first().and_then(|a| self.operand(st, func, a));
        match (callee, arg) {
            (Callee::Extern(id), Some(a)) if env.is_rounding(*id) => fact::rounded(a),
            (Callee::Extern(id), Some(a)) if env.is_abs(*id) => fact::abs(a),
            // A UTF-16 code unit, or -1 past the end (rt_abi.md).
            (Callee::Extern(id), _) if env.symbol(*id) == CHAR_CODE_AT => Fact::int(-1, 65535),
            _ => Fact::top(ty),
        }
    }

    fn solve(&mut self, func: &Function, env: &Env) {
        let n = func.blocks.len();
        let start = self.start(func, env);
        self.entry[0] = Some(self.live_in[0].iter().map(|&i| start[i as usize]).collect());
        let cfg = Cfg::of(func);
        let assigned = cfg.assigned(func, &self.slot, self.tys.len());
        let mut visits = vec![0u32; n];
        // Blocks in reverse postorder, so a loop's body settles before what follows it.
        let mut queued = vec![false; n];
        let mut work = BinaryHeap::from([Reverse(cfg.rank[0])]);
        queued[0] = true;
        // The state of the block being visited: the locals live into it loaded, the others
        // stale (`load`).
        let mut st = self.tops.clone();
        // The facts a branch refined, to restore before the other edge.
        let mut undo: Vec<(usize, Fact)> = vec![];
        while let Some(Reverse(r)) = work.pop() {
            let b = cfg.rpo[r as usize];
            queued[b] = false;
            if !self.load(b, &mut st) {
                continue;
            }
            self.work += (self.live_in[b].len() + func.blocks[b].stmts.len()) as u64;
            for s in &func.blocks[b].stmts {
                self.transfer(&mut st, func, s);
            }
            let mut arrive = |flow: &mut Flow, succ: usize, st: &State| {
                visits[succ] += 1;
                let widen = cfg.head[succ] && visits[succ] > WIDEN_AFTER;
                let widen = widen.then_some(assigned[succ].as_slice());
                if flow.merge(succ, st, widen) && !queued[succ] {
                    queued[succ] = true;
                    work.push(Reverse(cfg.rank[succ]));
                }
            };
            match &func.blocks[b].term {
                Terminator::Goto(t) => arrive(self, t.0 as usize, &st),
                Terminator::Branch { then, els, .. } => {
                    let cond = super::refine::condition(func, b);
                    for (target, taken) in [(then, true), (els, false)] {
                        let feasible = match &cond {
                            Some(c) => self.refine(&mut st, func, b, c, taken, &mut undo),
                            None => true,
                        };
                        if feasible {
                            arrive(self, target.0 as usize, &st);
                        }
                        self.work += undo.len() as u64;
                        for (i, f) in undo.drain(..).rev() {
                            st[i] = f;
                        }
                    }
                }
                Terminator::Switch {
                    value,
                    cases,
                    default,
                } => {
                    for (v, t) in cases {
                        self.set(&mut st, value, Fact::int(*v, *v), &mut undo);
                        arrive(self, t.0 as usize, &st);
                        for (i, f) in undo.drain(..).rev() {
                            st[i] = f;
                        }
                    }
                    arrive(self, default.0 as usize, &st);
                }
                t @ Terminator::Call { dest, next, .. } => {
                    if let Some(d) = dest.as_ref().filter(|d| d.proj.is_empty()) {
                        if let Some(i) = self.slot[d.local.0 as usize] {
                            st[i] = self.call_result(&st, func, env, t);
                        }
                    }
                    arrive(self, next.0 as usize, &st);
                }
                Terminator::Return(_) | Terminator::Unreachable => {}
            }
        }
    }

    /// The state at the function's entry: parameters every call sets to a constant have its
    /// facts.
    fn start(&self, func: &Function, env: &Env) -> State {
        let mut start: State = self.tys.iter().map(|t| Fact::top(*t)).collect();
        if let Some(params) = env.params.get(&func.symbol) {
            for (i, f) in params.iter().enumerate() {
                if let (Some(f), Some(Some(s))) = (f, self.slot.get(i)) {
                    start[*s] = *f;
                }
            }
        }
        for (i, f) in start.iter_mut().enumerate() {
            *f = self.capped(i, *f);
        }
        start
    }

    /// Join `out` (its locals live into `b`) into the entry state of `b`, widening the locals
    /// `widen` says the loop at `b` assigns; returns whether it changed. A local the loop does
    /// not assign keeps the value it enters with, so it needs no widening there: an outer loop's
    /// `k < 30` stays `[0, 29]` at an inner loop's head.
    fn merge(&mut self, b: usize, out: &State, widen: Option<&[bool]>) -> bool {
        let live = &self.live_in[b];
        self.work += live.len() as u64;
        let Some(old) = &mut self.entry[b] else {
            self.entry[b] = Some(live.iter().map(|&i| out[i as usize]).collect());
            return true;
        };
        let mut changed = false;
        for (o, &i) in old.iter_mut().zip(live) {
            let i = i as usize;
            let mut j = o.join(out[i]);
            if widen.is_some_and(|w| w[i]) && j != *o {
                j = widened(*o, j);
                if self.tys[i].is_int() {
                    let top = self.tops[i];
                    j.lo = j.lo.max(top.lo);
                    j.hi = j.hi.min(top.hi);
                }
            }
            if let Some(c) = self.caps[i] {
                j = meet(j, c);
            }
            changed |= j != *o;
            *o = j;
        }
        changed
    }

    /// Meet the fact of a tracked local operand with `v` (no-op for others), logging the fact
    /// it had in `undo`.
    fn set(&self, st: &mut State, op: &Operand, v: Fact, undo: &mut Vec<(usize, Fact)>) {
        if let Operand::Copy(p) = op {
            if let (true, Some(i)) = (p.proj.is_empty(), self.slot[p.local.0 as usize]) {
                undo.push((i, st[i]));
                st[i] = meet(st[i], v);
            }
        }
    }

    /// Refine `st` with the condition `c` of block `b` being `taken`; false if impossible.
    fn refine(
        &self,
        st: &mut State,
        func: &Function,
        b: usize,
        c: &super::refine::Condition,
        taken: bool,
        undo: &mut Vec<(usize, Fact)>,
    ) -> bool {
        let (Some(x), Some(y)) = (
            self.operand(st, func, &c.lhs),
            self.operand(st, func, &c.rhs),
        ) else {
            return true;
        };
        let Some((nx, ny)) = super::refine::refine(c.op, taken != c.negated, x, y) else {
            return false;
        };
        for (op, f) in [(&c.lhs, nx), (&c.rhs, ny)] {
            self.set(st, op, f, undo);
            for copy in super::refine::copies_of(func, b, op) {
                self.set(st, &Operand::Copy(Place::local(copy)), f, undo);
            }
            use super::refine::Conversion;
            match super::refine::converted_from(func, b, op, &self.preds) {
                Some((src, Conversion::Copy)) => {
                    self.set(st, &src, f, undo);
                    // `i = k as u64; t = i; t < xs.length`.
                    if let Some((k, Conversion::FromF64(ty))) =
                        super::refine::converted_from(func, b, &src, &self.preds)
                    {
                        self.set(st, &k, super::refine::unconverted(f, ty), undo);
                    }
                }
                // Strictly within ±2^53 the conversion was exact: `2^53 + 1` converts to 2^53.
                Some((src, Conversion::ToF64)) if f.magnitude() < TWO_53 => {
                    self.set(st, &src, f, undo)
                }
                // `k as u64 < xs.length`: `k` is below the length too.
                Some((src, Conversion::FromF64(ty))) => {
                    self.set(st, &src, super::refine::unconverted(f, ty), undo)
                }
                _ => {}
            }
        }
        true
    }
}

/// The order the solver visits blocks in, and where it widens.
struct Cfg {
    /// Blocks reachable from the entry, in reverse postorder.
    rpo: Vec<usize>,
    /// Per block: its position in `rpo` (unreachable blocks: past the end).
    rank: Vec<u32>,
    /// Per block: the target of a retreating edge, i.e. a loop head. Every cycle has one, so
    /// widening there bounds the iterations.
    head: Vec<bool>,
    /// Per loop head: the sources of its retreating edges.
    latches: Vec<Vec<usize>>,
    preds: Vec<Vec<usize>>,
}

impl Cfg {
    fn of(func: &Function) -> Cfg {
        let n = func.blocks.len();
        let succs: Vec<Vec<usize>> = func
            .blocks
            .iter()
            .map(|b| {
                crate::visit::successors(&b.term)
                    .iter()
                    .map(|s| s.0 as usize)
                    .collect()
            })
            .collect();
        // Iterative depth-first search: 1 = on the stack, 2 = finished.
        let mut state = vec![0u8; n];
        let mut head = vec![false; n];
        let mut latches = vec![vec![]; n];
        let mut post = Vec::with_capacity(n);
        let mut stack = vec![(0usize, 0usize)];
        state[0] = 1;
        while let Some((b, i)) = stack.last_mut() {
            let b = *b;
            if let Some(&s) = succs[b].get(*i) {
                *i += 1;
                match state[s] {
                    0 => {
                        state[s] = 1;
                        stack.push((s, 0));
                    }
                    1 => {
                        head[s] = true;
                        latches[s].push(b);
                    }
                    _ => {}
                }
            } else {
                state[b] = 2;
                post.push(b);
                stack.pop();
            }
        }
        let rpo: Vec<usize> = post.into_iter().rev().collect();
        let mut rank = vec![u32::MAX; n];
        for (i, &b) in rpo.iter().enumerate() {
            rank[b] = i as u32;
        }
        let mut preds = vec![vec![]; n];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s].push(b);
            }
        }
        Cfg {
            rpo,
            rank,
            head,
            latches,
            preds,
        }
    }

    /// Per block: for a loop head, which locals some block of its cycles assigns (`slot` maps
    /// locals to state indexes); empty for other blocks. The blocks are those that reach a
    /// retreating edge into the head without passing it (more, when the CFG is irreducible).
    fn assigned(&self, func: &Function, slot: &[Option<usize>], slots: usize) -> Vec<Vec<bool>> {
        let n = func.blocks.len();
        let mut out = vec![vec![]; n];
        for h in (0..n).filter(|&h| self.head[h]) {
            let mut defs = vec![false; slots];
            let mut seen = vec![false; n];
            seen[h] = true;
            let mut work = vec![h];
            for &u in &self.latches[h] {
                if !std::mem::replace(&mut seen[u], true) {
                    work.push(u);
                }
            }
            while let Some(b) = work.pop() {
                let mark = |l: Local, defs: &mut Vec<bool>| {
                    if let Some(i) = slot[l.0 as usize] {
                        defs[i] = true;
                    }
                };
                for st in &func.blocks[b].stmts {
                    if let Stmt::Assign(d, _) = st {
                        mark(d.local, &mut defs);
                    }
                }
                if let Terminator::Call { dest: Some(d), .. } = &func.blocks[b].term {
                    mark(d.local, &mut defs);
                }
                if b == h {
                    continue;
                }
                for &p in &self.preds[b] {
                    if !std::mem::replace(&mut seen[p], true) {
                        work.push(p);
                    }
                }
            }
            out[h] = defs;
        }
        out
    }
}

/// The type of an operand.
pub(super) fn operand_ty(func: &Function, op: &Operand) -> Ty {
    match op {
        Operand::Const(_, ty) => *ty,
        Operand::Copy(p) => match p.proj.last() {
            None => func.locals[p.local.0 as usize].ty,
            Some(velt_vir::vir::Proj::Deref(t)) => *t,
            Some(_) => Ty::Unit,
        },
    }
}

/// Both facts hold.
pub(super) fn meet(a: Fact, b: Fact) -> Fact {
    let integral = a.integral || b.integral;
    let (mut lo, mut hi) = (a.lo.max(b.lo), a.hi.min(b.hi));
    if integral {
        lo = lo.ceil();
        hi = hi.floor();
    }
    Fact {
        lo,
        hi,
        integral,
        nan: a.nan && b.nan,
        neg_zero: a.neg_zero && b.neg_zero,
    }
}

/// `new` (which holds `old`) with each bound that moved pushed out to the next threshold.
fn widened(old: Fact, new: Fact) -> Fact {
    let mut f = new;
    if new.hi > old.hi {
        f.hi = THRESHOLDS
            .iter()
            .copied()
            .find(|&t| t >= new.hi)
            .unwrap_or(f64::INFINITY);
    }
    if new.lo < old.lo {
        f.lo = THRESHOLDS
            .iter()
            .rev()
            .copied()
            .find(|&t| t <= new.lo)
            .unwrap_or(f64::NEG_INFINITY);
    }
    f
}

/// Is the binary operator a comparison?
pub(super) fn is_comparison(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}

#[cfg(test)]
#[path = "flow_tests.rs"]
mod tests;
