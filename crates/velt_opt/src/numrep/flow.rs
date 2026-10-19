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

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use velt_vir::vir::{
    AggId, BinOp, Callee, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty, UnOp,
};

use super::fact::{self, Fact, TWO_53};
use super::Env;

/// Arrivals at a loop head before its entry state is widened.
const WIDEN_AFTER: u32 = 2;
/// Largest tracked-locals × blocks product analysed (bounds memory and time).
const MAX_CELLS: usize = 1 << 19;
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

/// The largest array length: an array of 2^53 elements would not fit in memory.
const ARRAY_LEN_MAX: i128 = (1 << 53) - 1;

/// A fact per tracked local.
pub(super) type State = Vec<Fact>;

/// Block entry facts of one function.
pub(super) struct Flow {
    /// Local → index into a state, for tracked locals.
    slot: Vec<Option<usize>>,
    /// Type per tracked local.
    tys: Vec<Ty>,
    entry: Vec<Option<State>>,
    /// The arrays' aggregate (`Env::array`).
    array: Option<AggId>,
    /// Per tracked local: facts that hold for every value it takes (`counter`), met with its
    /// facts wherever it changes.
    caps: Vec<Option<Fact>>,
    /// Predecessors of each block.
    preds: Vec<Vec<usize>>,
}

impl Flow {
    /// Analyse `func`, tracking `tracked`. `None` when the function is too big.
    pub fn compute(func: &Function, env: &Env, tracked: &[Local]) -> Option<Flow> {
        if tracked.len().saturating_mul(func.blocks.len()) > MAX_CELLS {
            return None;
        }
        let mut slot = vec![None; func.locals.len()];
        for (i, l) in tracked.iter().enumerate() {
            slot[l.0 as usize] = Some(i);
        }
        let tys = tracked
            .iter()
            .map(|l| func.locals[l.0 as usize].ty)
            .collect();
        let mut flow = Flow {
            slot,
            tys,
            entry: vec![None; func.blocks.len()],
            array: env.array,
            caps: vec![None; tracked.len()],
            preds: crate::map_probe::region::predecessors(func),
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

    /// Facts about `l` before statement `si` of block `b` (`None` if unreachable or untracked).
    pub fn fact_before(&self, func: &Function, b: usize, si: usize, l: Local) -> Option<Fact> {
        let mut st = self.entry(b)?.clone();
        for s in &func.blocks[b].stmts[..si] {
            self.transfer(&mut st, func, s);
        }
        Some(st[self.slot(l)?])
    }

    /// `f` met with the cap of tracked local `i`.
    fn capped(&self, i: usize, f: Fact) -> Fact {
        match self.caps[i] {
            Some(c) if !f.is_empty() => meet(f, c),
            _ => f,
        }
    }

    /// Entry state of block `b` (`None` if unreachable).
    pub fn entry(&self, b: usize) -> Option<&State> {
        self.entry[b].as_ref()
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
            Operand::Copy(p) => {
                let ty = crate::visit::derefs(p).then(|| match p.proj.last() {
                    Some(velt_vir::vir::Proj::Deref(t)) => *t,
                    _ => Ty::Unit,
                })?;
                (ty.is_int() || ty.is_float()).then(|| Fact::top(ty))
            }
        }
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
        let arg = |op: &Operand| self.operand(st, func, op);
        let optype = |op: &Operand| operand_ty(func, op);
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
        self.entry[0] = Some(self.start(func, env));
        let cfg = Cfg::of(func);
        let assigned = cfg.assigned(func, &self.slot, self.tys.len());
        let mut visits = vec![0u32; n];
        // Blocks in reverse postorder, so a loop's body settles before what follows it.
        let mut queued = vec![false; n];
        let mut work = BinaryHeap::from([Reverse(cfg.rank[0])]);
        queued[0] = true;
        while let Some(Reverse(r)) = work.pop() {
            let b = cfg.rpo[r as usize];
            queued[b] = false;
            let Some(mut st) = self.entry[b].clone() else {
                continue;
            };
            for s in &func.blocks[b].stmts {
                self.transfer(&mut st, func, s);
            }
            for (succ, out) in self.edges(func, env, b, st) {
                visits[succ] += 1;
                let widen = cfg.head[succ] && visits[succ] > WIDEN_AFTER;
                let widen = widen.then_some(assigned[succ].as_slice());
                if self.merge(succ, out, widen) && !queued[succ] {
                    queued[succ] = true;
                    work.push(Reverse(cfg.rank[succ]));
                }
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

    /// Join `out` into the entry state of `b`, widening the locals `widen` says the loop at `b`
    /// assigns; returns whether it changed. A local the loop does not assign keeps the value it
    /// enters with, so it needs no widening there: an outer loop's `k < 30` stays `[0, 29]` at
    /// an inner loop's head.
    fn merge(&mut self, b: usize, out: State, widen: Option<&[bool]>) -> bool {
        let Some(old) = &mut self.entry[b] else {
            self.entry[b] = Some(out);
            return true;
        };
        let mut changed = false;
        for (i, (o, n)) in old.iter_mut().zip(out).enumerate() {
            let mut j = o.join(n);
            if widen.is_some_and(|w| w[i]) && j != *o {
                j = widened(*o, j);
                if self.tys[i].is_int() {
                    let top = Fact::top(self.tys[i]);
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

    /// Successor states of block `b` whose statements left `st`, refined by the branch.
    fn edges(&self, func: &Function, env: &Env, b: usize, st: State) -> Vec<(usize, State)> {
        match &func.blocks[b].term {
            Terminator::Goto(t) => vec![(t.0 as usize, st)],
            Terminator::Branch { then, els, .. } => {
                let cond = super::refine::condition(func, b);
                let mut out = vec![];
                for (target, taken) in [(then, true), (els, false)] {
                    let mut s = st.clone();
                    let feasible = match &cond {
                        Some(c) => self.refine(&mut s, func, b, c, taken),
                        None => true,
                    };
                    if feasible {
                        out.push((target.0 as usize, s));
                    }
                }
                out
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let mut out: Vec<(usize, State)> = cases
                    .iter()
                    .map(|(v, t)| {
                        let mut s = st.clone();
                        self.set(&mut s, value, Fact::int(*v, *v));
                        (t.0 as usize, s)
                    })
                    .collect();
                out.push((default.0 as usize, st));
                out
            }
            t @ Terminator::Call { dest, next, .. } => {
                let mut s = st;
                if let Some(d) = dest.as_ref().filter(|d| d.proj.is_empty()) {
                    if let Some(i) = self.slot[d.local.0 as usize] {
                        s[i] = self.call_result(&s, func, env, t);
                    }
                }
                vec![(next.0 as usize, s)]
            }
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        }
    }

    /// Meet the fact of a tracked local operand with `v` (no-op for others).
    pub(super) fn set(&self, st: &mut State, op: &Operand, v: Fact) {
        if let Operand::Copy(p) = op {
            if let (true, Some(i)) = (p.proj.is_empty(), self.slot[p.local.0 as usize]) {
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
            self.set(st, op, f);
            for copy in super::refine::copies_of(func, b, op) {
                self.set(st, &Operand::Copy(Place::local(copy)), f);
            }
            use super::refine::Conversion;
            match super::refine::converted_from(func, b, op, &self.preds) {
                Some((src, Conversion::Copy)) => {
                    self.set(st, &src, f);
                    // `i = k as u64; t = i; t < xs.length`.
                    if let Some((k, Conversion::FromF64(ty))) =
                        super::refine::converted_from(func, b, &src, &self.preds)
                    {
                        self.set(st, &k, super::refine::unconverted(f, ty));
                    }
                }
                // Strictly within ±2^53 the conversion was exact: `2^53 + 1` converts to 2^53.
                Some((src, Conversion::ToF64)) if f.magnitude() < TWO_53 => self.set(st, &src, f),
                // `k as u64 < xs.length`: `k` is below the length too.
                Some((src, Conversion::FromF64(ty))) => {
                    self.set(st, &src, super::refine::unconverted(f, ty))
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
