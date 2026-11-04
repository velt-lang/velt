//! Two `fits` tests of one index as one test (`fits_pair`).
//!
//! `while (i8x16.fits(a, i + 48) && i8x16.fits(b, i + 48))` keeps two exit tests: LLVM cannot
//! merge `fits`'s form (`len >= n && at <= len - n`) across arrays. Once the rounds have inlined
//! the prelude's forwarders, the pair is this, with nothing else in the four blocks but what
//! computes the second index:
//!
//! ```text
//! a:       _j1 = add _i, 48; _x = call fits(_a, _j1) -> a_test
//! a_test:  branch _x, r, out
//! r:       _j2 = add _i, 48; _y = call fits(_b, _j2) -> r_test
//! r_test:  branch _y, yes, out
//! ```
//!
//! and becomes `_x = call fits_both(_a, _b, _j1) -> a_test; a_test: branch _x, yes, out`. The two
//! agree on every length and index: `fits_both` is `fits` on `min(la, lb)`; `min >= n` holds when
//! both lengths are at least `n`, and then `min - n` does not wrap and is the smaller of `la - n`
//! and `lb - n`, so `at` is at most it when it is at most both. (Below `n`, both forms are false.)
//!
//! `fits` guards unchecked vector loads, so a wrong merge is a memory-safety bug. What makes this
//! one exact, each checked by [`matched`] and each with a test:
//! - **One lane type**: the same extern, by what its symbol means.
//! - **One index**: the two index operands have one value number, over the path from `a` to `r`'s
//!   call ([`values`]). Only register-like locals and integers count: a value read from memory
//!   or a local whose address was taken is never equal to anything.
//! - **One array value, no effect between**: `r` holds only assignments of pure arithmetic to
//!   register-like locals, so no store, call or trap sits between the tests, and `_b` has the
//!   same value at the end of `a` as at `r`'s call. An array `r` computes is not merged. That
//!   includes `fits(this.b, j)`: where the header is inline in the object, `r` computes an address
//!   (`_t = &(*_this).b`) with no load, and where the field holds a pointer to a shared array, `r`
//!   loads it; [`pure`] refuses both. Allowing the address needs a safety argument of its own, for
//!   `this` being valid where the first test fails.
//! - **Nothing else sees the difference**: `a_test`, `r` and `r_test` are entered only along this
//!   path; `_x` and `_y` are read only by their branches, and what `r` writes only in `r` (the new
//!   form never runs `r`, and leaves `_x` false where the first test alone was true); both
//!   `false` edges go to one block.
//!
//! The rewrite reads `_b`'s length also when `a`'s test fails. That read is safe: `_b` is the
//! same value there, and the language gives no way to make an array value's validity depend on
//! `fits(a, j)`. A test between them (`fits(a, j) && b !== null && fits(b, j)`) is a block between
//! them, which does not match; a dominating one still dominates. A constant second array other
//! than a static's address, which is how a null reaches here once constants have propagated, does
//! not match either.
//!
//! The pass goes over a function once, so the counts it asks about are taken once, at the first
//! pair of the right shape, and kept exact as it rewrites ([`Counts::rewritten`]): the blocks a
//! rewrite leaves dead stay in the function until `simplify_cfg` and still count, as they would
//! in a new scan.

#[cfg(test)]
mod tests;

use rustc_hash::FxHashMap;
use velt_vir::simd::{Lanes, Op, SimdCall};
use velt_vir::vir::{
    BinOp, BlockId, Callee, Const, ExternFn, ExternId, Function, Local, Operand, Place, Program,
    Rvalue, Stmt, Terminator, Ty,
};

use crate::locals::Usage;
use crate::visit::{derefs, stmt_places, successors, term_places, PlaceUse};

/// Rewrites every pair; returns the indices of the functions that changed, which hold dead blocks
/// until `simplify_cfg` runs.
pub(crate) fn run(program: &mut Program) -> Vec<usize> {
    let fits = fits_table(&program.externs);
    if fits.iter().all(Option::is_none) {
        return vec![];
    }
    let mut changed = vec![];
    for fi in 0..program.funcs.len() {
        let func = &mut program.funcs[fi];
        if rewrite_pairs(func, &mut program.externs, &fits).is_some_and(|c| c.rewrites > 0) {
            changed.push(fi);
        }
    }
    changed
}

/// Per extern: the lane type, if it is a `fits`.
fn fits_table(externs: &[ExternFn]) -> Vec<Option<Lanes>> {
    (externs.iter())
        .map(|e| match SimdCall::of_symbol(&e.symbol) {
            Some(SimdCall::Vector(lanes, Op::Fits)) => Some(lanes),
            _ => None,
        })
        .collect()
}

/// Rewrites the pairs of one function; the counts, if a pair of the right shape needed them.
fn rewrite_pairs(
    func: &mut Function,
    externs: &mut Vec<ExternFn>,
    fits: &[Option<Lanes>],
) -> Option<Counts> {
    let mut counts = None;
    for bi in 0..func.blocks.len() {
        let Some(pair) = matched(func, bi, fits, &mut counts) else {
            continue;
        };
        if let Some(counts) = &mut counts {
            counts.rewritten(&pair);
        }
        let both = fits_both(externs, pair.lanes);
        if let Terminator::Call { callee, args, .. } = &mut func.blocks[bi].term {
            *callee = Callee::Extern(both);
            *args = vec![pair.x, pair.y, pair.at];
        }
        func.blocks[pair.test.0 as usize].term = Terminator::Branch {
            cond: pair.cond,
            then: pair.yes,
            els: pair.no,
        };
    }
    counts
}

/// What one function's pairs ask about the whole function, taken once.
struct Counts {
    usage: Usage,
    /// Edges into each block, dead blocks' included.
    edges: Vec<u32>,
    rewrites: u32,
}

impl Counts {
    fn of(func: &Function) -> Counts {
        let mut edges = vec![0u32; func.blocks.len()];
        for s in func.blocks.iter().flat_map(|b| successors(&b.term)) {
            if let Some(n) = edges.get_mut(s.0 as usize) {
                *n += 1;
            }
        }
        Counts {
            usage: Usage::of(func),
            edges,
            rewrites: 0,
        }
    }

    /// Whether each of `blocks` has one way in (and is not the entry).
    fn entered_once(&self, blocks: &[BlockId]) -> bool {
        let once = |b: &BlockId| self.edges.get(b.0 as usize) == Some(&1);
        blocks.iter().all(once) && !blocks.contains(&BlockId(0))
    }

    /// `pair` is being rewritten: the first test's branch leaves `r` for `yes`, and its call also
    /// reads the second array. `r` and what follows stay in the function, so nothing else changes.
    fn rewritten(&mut self, pair: &Pair) {
        self.rewrites += 1;
        if let Some(n) = self.edges.get_mut(pair.r.0 as usize) {
            *n -= 1;
        }
        if let Some(n) = self.edges.get_mut(pair.yes.0 as usize) {
            *n += 1;
        }
        if let Operand::Copy(p) = &pair.y {
            if let Some(u) = self.usage.locals.get_mut(p.local.0 as usize) {
                u.reads += 1;
            }
        }
    }
}

/// The `fits_both` extern of `lanes`, declared when the program has none yet.
fn fits_both(externs: &mut Vec<ExternFn>, lanes: Lanes) -> ExternId {
    let symbol = format!("velt_simd_{}_fits_both", lanes.name());
    let found = externs.iter().position(|e| e.symbol == symbol);
    let id = found.unwrap_or_else(|| {
        externs.push(ExternFn {
            symbol,
            params: vec![Ty::Ptr, Ty::Ptr, Ty::U64],
            ret: Ty::Bool,
            noreturn: false,
            host: None,
            c_decl: None,
        });
        externs.len() - 1
    });
    ExternId(id as u32)
}

/// A pair of tests, starting at the block whose call is the first test.
struct Pair {
    lanes: Lanes,
    x: Operand,
    y: Operand,
    at: Operand,
    /// The block that branches on the first test, and what it branches on.
    test: BlockId,
    cond: Operand,
    /// The second test's block, which the rewrite leaves without a way in.
    r: BlockId,
    yes: BlockId,
    no: BlockId,
}

/// One `fits` call: what it tests, its result and the block after it.
struct FitsCall<'f> {
    lanes: Lanes,
    array: &'f Operand,
    at: &'f Operand,
    dest: Local,
    next: BlockId,
}

fn fits_call<'f>(t: &'f Terminator, fits: &[Option<Lanes>]) -> Option<FitsCall<'f>> {
    let Terminator::Call {
        callee: Callee::Extern(id),
        args,
        dest: Some(dest),
        next,
    } = t
    else {
        return None;
    };
    let lanes = (*fits.get(id.0 as usize)?)?;
    let [array, at] = args.as_slice() else {
        return None;
    };
    let dest = whole_local(dest)?;
    Some(FitsCall {
        lanes,
        array,
        at,
        dest,
        next: *next,
    })
}

fn whole_local(p: &Place) -> Option<Local> {
    p.proj.is_empty().then_some(p.local)
}

/// The two targets of a block that does nothing but branch on `cond`.
fn only_branch(func: &Function, b: BlockId, cond: Local) -> Option<(&Operand, BlockId, BlockId)> {
    let block = func.blocks.get(b.0 as usize)?;
    if !block.stmts.iter().all(|s| matches!(s, Stmt::Nop)) {
        return None;
    }
    let Terminator::Branch { cond: c, then, els } = &block.term else {
        return None;
    };
    let Operand::Copy(p) = c else {
        return None;
    };
    (whole_local(p) == Some(cond)).then_some((c, *then, *els))
}

/// The pair whose first test is the call ending block `bi`, if the rewrite is exact there (the
/// module's comment says why each condition is here).
fn matched(
    func: &Function,
    bi: usize,
    fits: &[Option<Lanes>],
    counts: &mut Option<Counts>,
) -> Option<Pair> {
    let block = func.blocks.get(bi)?;
    let first = fits_call(&block.term, fits)?;
    let (cond, r, no) = only_branch(func, first.next, first.dest)?;
    let second = fits_call(&func.blocks.get(r.0 as usize)?.term, fits)?;
    let (_, yes, second_no) = only_branch(func, second.next, second.dest)?;
    if second.lanes != first.lanes || second_no != no {
        return None;
    }
    let path = [BlockId(bi as u32), first.next, r, second.next];
    if (1..4).any(|i| path[..i].contains(&path[i])) {
        return None;
    }
    let counts = counts.get_or_insert_with(|| Counts::of(func));
    if !counts.entered_once(&path[1..]) {
        return None;
    }
    let usage = &counts.usage;
    // One local for both results is read twice, by the two branches.
    let read_once = |l: Local| {
        let u = usage.locals.get(l.0 as usize);
        u.is_some_and(|u| u.reads == 1 && !u.address_taken)
    };
    if !read_once(first.dest) || !read_once(second.dest) {
        return None;
    }
    let reads_in_r = reads_inside(func, r);
    let mut values = Values::new(usage);
    block.stmts.iter().for_each(|s| values.stmt(s));
    let at = values.operand(first.at);
    // Read at the first test now: a local that is the same there, or a static's address.
    if matches!(second.array, Operand::Const(c, _) if !matches!(c, Const::Static(_))) {
        return None;
    }
    let y = values.operand(second.array);
    // Nothing on the path reads the result (`read_once`); the table stays true regardless.
    values.write(first.dest);
    for s in &func.blocks[r.0 as usize].stmts {
        match s {
            Stmt::Nop => {}
            Stmt::Assign(dest, rv) => {
                let local = whole_local(dest)?;
                let u = usage.locals.get(local.0 as usize)?;
                let unread_outside = u.reads == *reads_in_r.get(&local.0).unwrap_or(&0);
                if !pure(rv) || !unread_outside || !usage.is_register(local) {
                    return None;
                }
                values.stmt(s);
            }
            _ => return None,
        }
    }
    if values.operand(second.at) != at || values.operand(second.array) != y {
        return None;
    }
    Some(Pair {
        lanes: first.lanes,
        x: first.array.clone(),
        y: second.array.clone(),
        at: first.at.clone(),
        test: first.next,
        cond: cond.clone(),
        r,
        yes,
        no,
    })
}

/// How many times the statements and the terminator of `r` read each local, counted as `Usage`
/// counts them.
fn reads_inside(func: &Function, r: BlockId) -> FxHashMap<u32, u32> {
    let mut reads = FxHashMap::default();
    let mut record = |p: &Place, use_: PlaceUse| {
        if derefs(p) || matches!(use_, PlaceUse::Read) {
            *reads.entry(p.local.0).or_insert(0) += 1;
        }
    };
    if let Some(b) = func.blocks.get(r.0 as usize) {
        b.stmts.iter().for_each(|s| stmt_places(s, &mut record));
        term_places(&b.term, &mut record);
    }
    reads
}

/// Whether an rvalue does nothing but compute from register-like locals and constants: it reads
/// no memory (no projection) and cannot trap (no division).
fn pure(rv: &Rvalue) -> bool {
    let plain = |o: &Operand| match o {
        Operand::Copy(p) => p.proj.is_empty(),
        Operand::Const(..) => true,
    };
    match rv {
        Rvalue::Use(a) | Rvalue::Cast(a, _) => plain(a),
        Rvalue::Binary(op, a, b) => wraps(*op) && plain(a) && plain(b),
        _ => false,
    }
}

/// Integer arithmetic with one result for its operands, and no trap.
fn wraps(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
    )
}

/// A value: equal numbers mean equal values, at any two points of the straight path walked.
type Value = u32;

/// What a value was computed as.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Expr {
    /// A register-like integer or pointer local, as of its `n`th write on the path.
    Local(u32, u32),
    Int(i128, Ty),
    Static(u32),
    Cast(Value, Ty),
    Binary(u8, Value, Value),
}

/// Local value numbering over the straight path from the first test's block to the second call.
struct Values<'u> {
    usage: &'u Usage,
    numbers: FxHashMap<Expr, Value>,
    /// Per local written on the path: how many writes so far, and its value now.
    locals: FxHashMap<u32, (u32, Option<Value>)>,
    next: Value,
}

impl<'u> Values<'u> {
    fn new(usage: &'u Usage) -> Self {
        Values {
            usage,
            numbers: FxHashMap::default(),
            locals: FxHashMap::default(),
            next: 0,
        }
    }

    /// A value equal to no other.
    fn fresh(&mut self) -> Value {
        self.next += 1;
        self.next
    }

    fn number(&mut self, e: Expr) -> Value {
        if let Some(v) = self.numbers.get(&e) {
            return *v;
        }
        let v = self.fresh();
        self.numbers.insert(e, v);
        v
    }

    fn operand(&mut self, o: &Operand) -> Value {
        match o {
            Operand::Const(Const::Int(n), ty) => self.number(Expr::Int(*n, *ty)),
            Operand::Const(Const::Static(s), _) => self.number(Expr::Static(s.0)),
            Operand::Copy(p) if p.proj.is_empty() => self.local(p.local),
            Operand::Copy(_) | Operand::Const(..) => self.fresh(),
        }
    }

    fn local(&mut self, l: Local) -> Value {
        if !self.usage.is_register(l) {
            return self.fresh();
        }
        let (writes, value) = self.locals.get(&l.0).copied().unwrap_or((0, None));
        match value {
            Some(v) => v,
            None => self.number(Expr::Local(l.0, writes)),
        }
    }

    /// `l` was written with something unknown.
    fn write(&mut self, l: Local) {
        let entry = self.locals.entry(l.0).or_insert((0, None));
        *entry = (entry.0 + 1, None);
    }

    fn stmt(&mut self, s: &Stmt) {
        let Stmt::Assign(dest, rv) = s else {
            return;
        };
        let Some(l) = whole_local(dest) else {
            return;
        };
        let value = self.rvalue(rv);
        self.write(l);
        if let Some(entry) = self.locals.get_mut(&l.0) {
            entry.1 = value;
        }
    }

    fn rvalue(&mut self, rv: &Rvalue) -> Option<Value> {
        if !pure(rv) {
            return None;
        }
        Some(match rv {
            Rvalue::Use(a) => self.operand(a),
            Rvalue::Cast(a, to) => {
                let a = self.operand(a);
                self.number(Expr::Cast(a, *to))
            }
            Rvalue::Binary(op, a, b) => {
                let (a, b) = (self.operand(a), self.operand(b));
                self.number(Expr::Binary(*op as u8, a, b))
            }
            _ => return None,
        })
    }
}
