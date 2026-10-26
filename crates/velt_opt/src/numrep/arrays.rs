//! Arrays of whole numbers (#1599, design #525 step 5): a `number[]` that never leaves its
//! function and only ever holds whole numbers within ±2^53 stores its elements as `i64`.
//!
//! - **Groups**: after `sroa`, a local array is three locals (data pointer, length, capacity)
//!   and its elements are reached through the data pointer. A group is the pointers connected
//!   by copies and `ptradd`, and it qualifies when nothing else can reach its memory: every
//!   pointer is set from null, from `velt_rt_alloc`, from `velt_rt_realloc` of the group, a
//!   copy or a `ptradd` of the group; and is only copied within the group, offset, compared,
//!   handed to `velt_rt_realloc` / `velt_rt_free`, or dereferenced as one element type (`f64`,
//!   or `i64` once retyped). Every element the program reads was then stored by a statement
//!   of the function (a read past the length is bounds-checked, never a read of fresh memory).
//! - **Facts**: an element is any value some store writes. The facts are a fixpoint: loads
//!   start as 0, `Flow` runs with them, the stored values are joined in, and the loop repeats
//!   until the stores add nothing. After two rounds a bound that still moves widens to the
//!   largest whole number an `i64` element holds exactly, and a group whose facts can no
//!   longer be stored as integers (a value grows past that, a fraction, NaN or `-0`) drops
//!   out: its loads are unknown from then on. Any facts the stores stay within are sound, by
//!   induction over a run: until the first store of a value outside the facts, every load
//!   returns a value inside them, so the analysis holds up to that store and has seen it.
//! - **Retyping**: an `f64` group whose facts are whole, never NaN, never `-0` and within
//!   ±2^53 stores `cast v as i64` and loads `cast (*p as i64) as f64`, both exact. The element
//!   size stays 8 bytes, so allocation, growth and freeing are unchanged, and zeroed memory is
//!   0 either way. `narrow` then computes with the integers directly: a load feeding an index
//!   or another narrowed value needs no conversion, and a copy between elements stays a copy.

use velt_vir::vir::{
    BinOp, Callee, Const, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use super::fact::{Fact, TWO_53};
use super::flow::{Flow, State};
use super::Env;
use crate::locals::Usage;
use crate::srclocs::{push_stmt, rewrite_stmts};
use crate::visit::{stmt_operands_mut, term_operands_mut};

/// The allocator's entry points (rt_abi.md).
const ALLOC: &str = "velt_rt_alloc";
const REALLOC: &str = "velt_rt_realloc";
const FREE: &str = "velt_rt_free";

/// Fixpoint rounds before giving up on a group's facts.
const MAX_ROUNDS: usize = 8;
/// Rounds that join the stored values before later ones widen them.
const JOIN_ROUNDS: usize = 2;
/// The largest whole number an `i64` element holds exactly (`Fact::exact_int`).
const MAX_EXACT: f64 = TWO_53 - 1.0;

/// The element pointer groups of one function.
pub(super) struct Groups {
    /// Per local: its group, when it is a pointer of a qualifying group.
    of: Vec<Option<usize>>,
    /// Per group: the element type its loads and stores use.
    ty: Vec<Ty>,
}

impl Groups {
    /// The group of the elements `p` reads or writes (`*q` with `q` in a group).
    pub fn of_element(&self, p: &Place) -> Option<usize> {
        match p.proj[..] {
            [Proj::Deref(_)] => self.of.get(p.local.0 as usize).copied().flatten(),
            _ => None,
        }
    }

    /// Per local: the facts of the elements its group points at.
    pub fn per_local(&self, facts: &[Option<Fact>]) -> Vec<Option<Fact>> {
        self.of.iter().map(|g| g.and_then(|g| facts[g])).collect()
    }

    /// The same groups of the same locals as `other` (locals added since, outside any group,
    /// make no difference).
    pub fn same(&self, other: &Groups) -> bool {
        let members = |g: &Groups| {
            let end = g.of.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
            g.of[..end].to_vec()
        };
        self.ty == other.ty && members(self) == members(other)
    }
}

/// Union-find over locals.
struct Sets(Vec<usize>);

impl Sets {
    fn find(&mut self, i: usize) -> usize {
        let mut r = i;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut i = i;
        while self.0[i] != r {
            let next = self.0[i];
            self.0[i] = r;
            i = next;
        }
        r
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        self.0[a] = b;
    }
}

/// What the walk in `groups` has seen of each local.
struct Seen {
    /// The local's memory may be reached some other way (or it is not a pointer we follow).
    bad: Vec<bool>,
    /// The element type of the local's dereferences (`Unit` while there are none).
    elem: Vec<Ty>,
}

impl Seen {
    /// A place read or written: a dereference must be one whole element of `f64` or `i64`.
    fn place(&mut self, p: &Place) {
        if !crate::visit::derefs(p) {
            return;
        }
        let i = p.local.0 as usize;
        match p.proj[..] {
            [Proj::Deref(t @ (Ty::F64 | Ty::I64))]
                if self.elem[i] == Ty::Unit || self.elem[i] == t =>
            {
                self.elem[i] = t;
            }
            _ => self.bad[i] = true,
        }
    }

    /// An operand outside the forms `groups` accepts: a whole pointer escapes there.
    fn operand(&mut self, op: &Operand) {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() {
                self.bad[p.local.0 as usize] = true;
            } else {
                self.place(p);
            }
        }
    }
}

/// The groups of `func` whose memory only the function's own loads and stores reach.
pub(super) fn groups(env: &Env, func: &Function) -> Groups {
    let n = func.locals.len();
    let usage = Usage::of(func);
    let ptr = |l: Local| {
        let i = l.0 as usize;
        i >= func.params.len() && func.locals[i].ty == Ty::Ptr && usage.is_register(l)
    };
    let whole = |op: &Operand| match op {
        Operand::Copy(p) if p.proj.is_empty() && ptr(p.local) => Some(p.local.0 as usize),
        _ => None,
    };
    let mut sets = Sets((0..n).collect());
    let mut seen = Seen {
        bad: vec![false; n],
        elem: vec![Ty::Unit; n],
    };
    for b in &func.blocks {
        for s in &b.stmts {
            match s {
                Stmt::Assign(d, rv) if d.proj.is_empty() && ptr(d.local) => {
                    let di = d.local.0 as usize;
                    match rv {
                        Rvalue::Use(Operand::Const(Const::Int(0), _)) => {}
                        Rvalue::Use(op) if whole(op).is_some() => {
                            sets.union(di, whole(op).unwrap_or(di));
                        }
                        Rvalue::Binary(BinOp::PtrAdd, base, off) if whole(base).is_some() => {
                            sets.union(di, whole(base).unwrap_or(di));
                            seen.operand(off);
                        }
                        _ => {
                            seen.bad[di] = true;
                            crate::visit::rvalue_operands(rv, &mut |op| seen.operand(op));
                        }
                    }
                }
                Stmt::Assign(d, rv) => {
                    seen.place(d);
                    match rv {
                        // Comparing pointers (a null test) reveals nothing of the memory.
                        Rvalue::Binary(BinOp::Eq | BinOp::Ne, a, b) => {
                            for op in [a, b] {
                                if whole(op).is_none() {
                                    seen.operand(op);
                                }
                            }
                        }
                        Rvalue::AddrOf(p) => seen.bad[p.local.0 as usize] = true,
                        _ => crate::visit::rvalue_operands(rv, &mut |op| seen.operand(op)),
                    }
                }
                s => crate::visit::stmt_operands(s, &mut |op| seen.operand(op)),
            }
        }
        // A call's result written straight into an element would be a store the facts miss.
        let call_dest = |d: &Option<Place>, seen: &mut Seen| {
            if let Some(d) = d {
                seen.bad[d.local.0 as usize] |= !d.proj.is_empty();
            }
        };
        match &b.term {
            Terminator::Call {
                callee: Callee::Extern(id),
                args,
                dest,
                ..
            } if matches!(env.symbol(*id), ALLOC | REALLOC | FREE) => {
                let sym = env.symbol(*id);
                for (k, a) in args.iter().enumerate() {
                    match whole(a) {
                        Some(_) if k == 0 && sym == FREE => {}
                        Some(i) if k == 0 && sym == REALLOC => match dest {
                            Some(d) if d.proj.is_empty() && ptr(d.local) => {
                                sets.union(d.local.0 as usize, i);
                            }
                            // The new pointer reaches the memory through a local the groups
                            // do not follow.
                            _ => seen.bad[i] = true,
                        },
                        _ => seen.operand(a),
                    }
                }
                call_dest(dest, &mut seen);
            }
            t => {
                crate::visit::term_operands(t, &mut |op| seen.operand(op));
                if let Terminator::Call { dest, .. } = t {
                    // Any other call's pointer result is memory the function did not allocate.
                    if let Some(d) = dest {
                        seen.bad[d.local.0 as usize] = true;
                    }
                    call_dest(dest, &mut seen);
                }
            }
        }
    }
    let Seen { bad, elem } = seen;
    // A group qualifies when no member is bad and its members agree on one element type.
    let mut root_bad = vec![false; n];
    let mut root_ty = vec![Ty::Unit; n];
    for i in 0..n {
        if !ptr(Local(i as u32)) {
            continue;
        }
        let r = sets.find(i);
        root_bad[r] |= bad[i];
        match (root_ty[r], elem[i]) {
            (_, Ty::Unit) => {}
            (Ty::Unit, t) => root_ty[r] = t,
            (a, b) if a == b => {}
            _ => root_bad[r] = true,
        }
    }
    let mut index = vec![None; n];
    let mut groups = Groups {
        of: vec![None; n],
        ty: vec![],
    };
    for i in 0..n {
        if !ptr(Local(i as u32)) {
            continue;
        }
        let r = sets.find(i);
        if root_bad[r] || root_ty[r] == Ty::Unit {
            continue;
        }
        let g = *index[r].get_or_insert_with(|| {
            groups.ty.push(root_ty[r]);
            groups.ty.len() - 1
        });
        groups.of[i] = Some(g);
    }
    groups
}

/// The groups of a function and the facts of their elements, computed once per `numrep` run:
/// retyping a group keeps its pointers and the values it stores, so both stay true of the
/// retyped function.
pub(super) struct Elements {
    pub groups: Groups,
    /// Per group: the facts of its elements (`None` when they are not known: too big a
    /// function, no fixpoint, or no store at all).
    facts: Vec<Option<Fact>>,
}

impl Elements {
    /// `groups` with nothing known of their elements.
    pub fn unknown(groups: Groups) -> Elements {
        let facts = vec![None; groups.ty.len()];
        Elements { groups, facts }
    }

    /// The facts of the elements of `groups`, computed with `Flow` over `tracked`; with the
    /// last `Flow`, when it ran with exactly these facts.
    pub fn of(
        env: &Env,
        func: &Function,
        tracked: &[Local],
        groups: Groups,
    ) -> (Elements, Option<Flow>) {
        let k = groups.ty.len();
        if k == 0 {
            return (Elements::unknown(groups), None);
        }
        // Any facts the stores stay within are sound (module docs); starting from 0 rather
        // than from no value keeps `xs[i] + 5` from looking like NaN (an operand without
        // values) in the first round. Zeroed memory is 0 too.
        // `None`: the group dropped out, its elements unknown.
        let mut elems = vec![Some(Fact::int(0, 0)); k];
        for round in 0..MAX_ROUNDS {
            let Some(flow) = Flow::compute(func, env, tracked, groups.per_local(&elems)) else {
                return (Elements::unknown(groups), None);
            };
            let stored = stored(func, &flow, &groups);
            let settled = |e: &Option<Fact>, s: &Fact| e.is_none_or(|e| e.join(*s) == e);
            if elems.iter().zip(&stored).all(|(e, s)| settled(e, s)) {
                // A group no reachable store writes has nothing to narrow.
                let all_stored = stored.iter().all(|s| !s.is_empty());
                let facts = elems
                    .into_iter()
                    .zip(stored)
                    .map(|(e, s)| e.filter(|_| !s.is_empty()))
                    .collect();
                return (Elements { groups, facts }, all_stored.then_some(flow));
            }
            for (e, s) in elems.iter_mut().zip(stored) {
                let Some(old) = *e else { continue };
                let joined = old.join(s);
                let next = if round < JOIN_ROUNDS {
                    joined
                } else {
                    widened(old, joined)
                };
                // Facts only grow: once they cannot be stored as integers, they never will.
                *e = (next.exact_int() && !next.neg_zero).then_some(next);
            }
        }
        (Elements::unknown(groups), None)
    }

    /// Per local: the facts of the elements its group points at.
    pub fn per_local(&self) -> Vec<Option<Fact>> {
        self.groups.per_local(&self.facts)
    }

    /// Per group: may its elements be stored as `i64`?
    fn eligible(&self) -> Vec<bool> {
        self.groups
            .ty
            .iter()
            .zip(&self.facts)
            .map(|(ty, f)| *ty == Ty::F64 && f.is_some_and(|f| f.exact_int() && !f.neg_zero))
            .collect()
    }
}

/// `new` (which holds `old`) with each bound that moved pushed out to `MAX_EXACT`, unless it
/// is already past it.
fn widened(old: Fact, new: Fact) -> Fact {
    let mut f = new;
    if new.hi > old.hi {
        f.hi = f.hi.max(MAX_EXACT);
    }
    if new.lo < old.lo {
        f.lo = f.lo.min(-MAX_EXACT);
    }
    f
}

/// Per group: the join of the values its stores write, under `flow`.
fn stored(func: &Function, flow: &Flow, groups: &Groups) -> Vec<Fact> {
    let mut out = vec![Fact::empty(); groups.ty.len()];
    for (bi, b) in func.blocks.iter().enumerate() {
        let Some(entry) = flow.entry(bi) else {
            continue;
        };
        let mut st: State = entry.clone();
        for s in &b.stmts {
            if let Stmt::Assign(d, rv) = s {
                if let Some(g) = groups.of_element(d) {
                    out[g] = out[g].join(flow.rvalue(&st, func, rv, groups.ty[g]));
                }
            }
            flow.transfer(&mut st, func, s);
        }
    }
    out
}

/// Store the elements of the groups `chosen` (all eligible) as `i64`.
fn retype(func: &mut Function, groups: &Groups, chosen: &[bool]) {
    let mut rt = Retyper {
        groups,
        chosen,
        base: func.locals.len(),
        temps: vec![],
    };
    for bi in 0..func.blocks.len() {
        rewrite_stmts(func, bi, |s, out| rt.stmt(s, out));
        let mut term = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable);
        let mut stmts = vec![];
        term_operands_mut(&mut term, &mut |op| rt.load(op, &mut stmts));
        for s in stmts {
            push_stmt(func, bi, s, None);
        }
        func.blocks[bi].term = term;
    }
    func.locals
        .extend(rt.temps.into_iter().map(|ty| LocalDecl::new(ty, None)));
}

struct Retyper<'g> {
    groups: &'g Groups,
    /// Per group: retyped to `i64`.
    chosen: &'g [bool],
    /// Index of the first temporary.
    base: usize,
    temps: Vec<Ty>,
}

impl Retyper<'_> {
    fn temp(&mut self, ty: Ty) -> Local {
        self.temps.push(ty);
        Local((self.base + self.temps.len() - 1) as u32)
    }

    /// Is `p` an element of a retyped group?
    fn retyped(&self, p: &Place) -> bool {
        self.groups.of_element(p).is_some_and(|g| self.chosen[g])
    }

    /// `*q as f64` → `*q as i64` for an element of a retyped group; returns whether it was one.
    fn retype_place(&self, p: &mut Place) -> bool {
        if !self.retyped(p) {
            return false;
        }
        p.proj = vec![Proj::Deref(Ty::I64)];
        true
    }

    /// A load of a retyped element becomes a temporary converted back to `f64` (exact).
    fn load(&mut self, op: &mut Operand, out: &mut Vec<Stmt>) {
        let Operand::Copy(p) = op else { return };
        let mut p = p.clone();
        if !self.retype_place(&mut p) {
            return;
        }
        let t = self.temp(Ty::F64);
        out.push(Stmt::Assign(
            Place::local(t),
            Rvalue::Cast(Operand::Copy(p), Ty::F64),
        ));
        *op = Operand::Copy(Place::local(t));
    }

    fn stmt(&mut self, s: Stmt, out: &mut Vec<Stmt>) {
        match s {
            Stmt::Assign(mut d, rv) if self.retyped(&d) => {
                self.retype_place(&mut d);
                let v = match rv {
                    // A copy between retyped elements stays a copy.
                    Rvalue::Use(Operand::Copy(mut q)) if self.retyped(&q) => {
                        self.retype_place(&mut q);
                        Operand::Copy(q)
                    }
                    Rvalue::Use(Operand::Const(Const::Float(x), _)) => {
                        Operand::Const(Const::Int(x as i128), Ty::I64)
                    }
                    Rvalue::Use(Operand::Const(Const::Int(x), _)) => {
                        Operand::Const(Const::Int(x), Ty::I64)
                    }
                    Rvalue::Use(mut op) => {
                        self.load(&mut op, out);
                        self.as_int(op, out)
                    }
                    mut rv => {
                        crate::visit::rvalue_operands_mut(&mut rv, &mut |op| self.load(op, out));
                        let f = self.temp(Ty::F64);
                        out.push(Stmt::Assign(Place::local(f), rv));
                        self.as_int(Operand::Copy(Place::local(f)), out)
                    }
                };
                out.push(Stmt::Assign(d, Rvalue::Use(v)));
            }
            mut s => {
                stmt_operands_mut(&mut s, &mut |op| self.load(op, out));
                out.push(s);
            }
        }
    }

    /// The whole `f64` value `op` as an `i64` (exact: the group's facts bound it).
    fn as_int(&mut self, op: Operand, out: &mut Vec<Stmt>) -> Operand {
        let t = self.temp(Ty::I64);
        out.push(Stmt::Assign(Place::local(t), Rvalue::Cast(op, Ty::I64)));
        Operand::Copy(Place::local(t))
    }
}

/// Eligible groups beyond which `choose` does not try each group alone.
const MAX_TRIALS: usize = 4;

/// `func` narrowed (`narrowed`: `numrep`'s plan and rewrite, and whether it changed anything),
/// with the eligible groups of `elements` retyped where that keeps fewer conversions between
/// floats and integers than doubles do, each weighted by its loop depth: all of them when
/// that pays; otherwise the groups that pay alone (with 2 to `MAX_TRIALS` eligible). An
/// element only ever copied into an array of doubles, or added to a double, would need a
/// conversion per read that the doubles did not (#1599). Retyping is only tried when the
/// narrowed doubles convert an element where it is loaded or stored.
pub(super) fn choose(
    env: &Env,
    func: &Function,
    elements: &Elements,
    narrowed: impl Fn(Function) -> (Function, bool),
) -> (Function, bool) {
    let base = narrowed(func.clone());
    let eligible = elements.eligible();
    if !eligible.contains(&true) || !converts_elements(env, &base.0, &elements.groups, &eligible) {
        return base;
    }
    let groups = &elements.groups;
    let depth = loop_depths(func);
    let base_cost = conversions(env, &base.0, &depth);
    let trial = |chosen: &[bool]| {
        let mut f = func.clone();
        retype(&mut f, groups, chosen);
        let (f, _) = narrowed(f);
        let cost = conversions(env, &f, &depth);
        (f, cost)
    };
    let (all, cost) = trial(&eligible);
    if cost < base_cost {
        return (all, true);
    }
    let count = eligible.iter().filter(|&&e| e).count();
    if !(2..=MAX_TRIALS).contains(&count) {
        return base;
    }
    let mut keep = vec![false; eligible.len()];
    let mut last = None;
    for g in (0..eligible.len()).filter(|&g| eligible[g]) {
        let mut only = vec![false; eligible.len()];
        only[g] = true;
        let (one, cost) = trial(&only);
        if cost < base_cost {
            keep[g] = true;
            last = Some(one);
        }
    }
    match keep.iter().filter(|&&k| k).count() {
        0 => base,
        1 => (last.unwrap_or(base.0), true),
        _ => (trial(&keep).0, true),
    }
}

/// Does `func` (narrowed with the elements as doubles) convert an element of an `eligible`
/// group between float and integer, or round or check it, where it is loaded or stored? Only
/// those conversions can retyping remove.
fn converts_elements(env: &Env, func: &Function, groups: &Groups, eligible: &[bool]) -> bool {
    let element = |p: &Place| groups.of_element(p).is_some_and(|g| eligible[g]);
    // Locals loaded from such an element, and locals stored into one.
    let mut loaded = vec![false; func.locals.len()];
    let mut stored = vec![false; func.locals.len()];
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        match s {
            Stmt::Assign(d, Rvalue::Use(Operand::Copy(p))) if d.proj.is_empty() && element(p) => {
                loaded[d.local.0 as usize] = true;
            }
            Stmt::Assign(d, Rvalue::Use(Operand::Copy(p))) if p.proj.is_empty() && element(d) => {
                stored[p.local.0 as usize] = true;
            }
            _ => {}
        }
    }
    let from_element = |op: &Operand| match op {
        Operand::Copy(p) if p.proj.is_empty() => loaded[p.local.0 as usize],
        Operand::Copy(p) => element(p),
        Operand::Const(..) => false,
    };
    let ty = |op: &Operand| super::flow::operand_ty(func, op);
    func.blocks.iter().any(|b| {
        let casts = b.stmts.iter().any(|s| match s {
            Stmt::Assign(d, Rvalue::Cast(op, to)) => {
                let from = ty(op);
                (from.is_float() && to.is_int() && from_element(op))
                    || (from.is_int()
                        && to.is_float()
                        && (element(d) || (d.proj.is_empty() && stored[d.local.0 as usize])))
            }
            _ => false,
        });
        casts
            || matches!(&b.term, Terminator::Call { callee, args, .. }
                if float_call(env, callee) && args.iter().any(from_element))
    })
}

/// Does `callee` round a float, take its ToInt32 or index with it?
fn float_call(env: &Env, callee: &Callee) -> bool {
    match callee {
        Callee::Extern(id) => env.is_rounding(*id) || env.is_to_int32(callee),
        c => env.is_float_index(c),
    }
}

/// Loop levels a conversion is assumed to repeat: each multiplies its weight by `PER_LEVEL`.
const MAX_DEPTH: u32 = 6;
const PER_LEVEL: u64 = 8;

/// The conversions between floats and integers in `func` (casts, and the calls that round or
/// check a float), each weighted by `PER_LEVEL` to the loop depth of its block in `depth` (the
/// function's blocks before narrowing; blocks added since count as depth 0).
fn conversions(env: &Env, func: &Function, depth: &[u32]) -> u64 {
    let ty = |op: &Operand| super::flow::operand_ty(func, op);
    let mut total = 0;
    for (bi, b) in func.blocks.iter().enumerate() {
        let w = PER_LEVEL.pow(depth.get(bi).copied().unwrap_or(0).min(MAX_DEPTH));
        for s in &b.stmts {
            if let Stmt::Assign(_, Rvalue::Cast(op, to)) = s {
                let from = ty(op);
                if (from.is_float() && to.is_int()) || (from.is_int() && to.is_float()) {
                    total += w;
                }
            }
        }
        if matches!(&b.term, Terminator::Call { callee, .. } if float_call(env, callee)) {
            total += w;
        }
    }
    total
}

/// Per block of `func`: how many loops it is nested in (cycles of the CFG, found as strongly
/// connected components, each peeled of its entry and searched again).
fn loop_depths(func: &Function) -> Vec<u32> {
    let n = func.blocks.len();
    let succ: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| {
            crate::visit::successors(&b.term)
                .into_iter()
                .map(|t| t.0 as usize)
                .collect()
        })
        .collect();
    let mut depth = vec![0u32; n];
    let mut work: Vec<Vec<usize>> = vec![(0..n).collect()];
    while let Some(nodes) = work.pop() {
        let mut index = vec![usize::MAX; n];
        for (i, &v) in nodes.iter().enumerate() {
            index[v] = i;
        }
        let edges: Vec<Vec<usize>> = nodes
            .iter()
            .map(|&v| {
                succ[v]
                    .iter()
                    .filter(|&&t| index[t] != usize::MAX)
                    .map(|&t| index[t])
                    .collect()
            })
            .collect();
        for scc in crate::scc::sccs(&edges) {
            if scc.len() == 1 && !edges[scc[0]].contains(&scc[0]) {
                continue;
            }
            let members: Vec<usize> = scc.iter().map(|&i| nodes[i]).collect();
            for &v in &members {
                depth[v] += 1;
            }
            // The loop's entry: a member with a predecessor outside it (else its first block).
            let inside = |v: usize| members.contains(&v);
            let head = members
                .iter()
                .copied()
                .find(|&h| (0..n).any(|p| !inside(p) && succ[p].contains(&h)))
                .unwrap_or(members[0]);
            work.push(members.into_iter().filter(|&v| v != head).collect());
        }
    }
    depth
}
