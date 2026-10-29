//! Read-only copies of counted pointers borrow their source instead of sharing it (#1715).
//!
//! `const data = this.db.data;` gives the local its own reference to the array: lowering
//! increments the object's count word (`retain`, `lower/rc.rs`) and releases it where the local
//! dies (the count is 1: free it, else decrement). When nothing can free the object while the
//! local is still used, neither is needed: the source's own reference keeps the object alive,
//! as it does for call arguments (docs/reference/memory.md "calls borrow"). The same holds for
//! any counted object (arrays, class objects) behind a pointer.
//!
//! A *family* is the pointer register the increment is made through (`r`, written once) and
//! the locals its value is copied into; *count pointers* are the registers holding a member's
//! address less 8 (the count word). A family is borrowed when, in the blocks that stay
//! reachable once its releases skip their freeing path:
//! - its increment is the only one, and its releases are the only other code that touches the
//!   count words: a branch on `count == 1` whose other side stores `count - 1`;
//! - its values never leave it: a member is copied only into members, compared, read and
//!   written through, and passed to calls (which borrow it); it is never stored, returned, cast
//!   or put into an aggregate, and its address is never taken. Members other than `r` are only
//!   written with member values or null;
//! - every path from the increment to a return passes a release (a member's null test that
//!   finds it null counts): so no call took the reference over, and no member was overwritten
//!   while holding it;
//! - no call that may free a counted object runs while a member is still used after it. Only
//!   calls free: a release that drops the last reference frees the object (a runtime call), and
//!   so does drop glue. Calls that cannot: runtime functions that only read, allocation and
//!   buffer reallocation (a buffer is never a counted object), calls that never return and are
//!   not handed a member (the family is not used after them; one that is handed a member runs
//!   with it, so it counts as freeing when its body may free at all), and Velt functions that
//!   make only such calls (`Frees`, computed callees first, after their own families are
//!   borrowed).
//!
//! Then the increment and each release's decrement become `Nop`s and each release's count test
//! goes straight to the decrement's block: the freeing path is no longer reached and
//! `simplify_cfg` removes it.

#[cfg(test)]
mod tests;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    BinOp, BlockId, Callee, Const, Function, Local, Operand, Place, Program, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::callgraph::CallGraph;
use crate::locals::Usage;

/// Runtime functions that free no counted object, besides `super::READ_ONLY`.
const NO_FREE: [&str; 2] = ["velt_rt_alloc", "velt_rt_realloc"];

/// Whether a call of each callee may free a counted object.
pub(crate) struct Frees {
    /// Per function: whether its body makes a call that may free, on any path (also one that
    /// then never returns).
    funcs: Vec<bool>,
    /// Per function: whether some path through it returns (one imported from another unit,
    /// with no body here, may).
    returns: Vec<bool>,
    externs: Vec<bool>,
}

impl Frees {
    /// Whether a call of `callee` may free a counted object while the caller's family is used:
    /// `handed` when the call is handed a member (the callee runs with it), else only when it
    /// is used after the call. A callee that never returns may free only while it runs, so it
    /// matters only when handed a member.
    fn may_free(&self, callee: &Callee, handed: bool) -> bool {
        match callee {
            Callee::Func(f) => {
                let i = f.0 as usize;
                (handed || self.returns.get(i).copied().unwrap_or(true))
                    && self.funcs.get(i).copied().unwrap_or(true)
            }
            Callee::Extern(e) => self.externs.get(e.0 as usize).copied().unwrap_or(true),
            Callee::Ptr { .. } => true,
        }
    }

    /// Whether `func` makes a call that may free (see the module doc). Its callers may hand it
    /// a member, which it may hand on: a callee that never returns counts here too.
    fn frees(&self, func: &Function) -> bool {
        func.blocks.iter().any(|b| match &b.term {
            Terminator::Call { callee, .. } => self.may_free(callee, true),
            _ => false,
        })
    }
}

/// Borrow the families of every function, callees first; returns which functions changed.
pub(crate) fn run(program: &mut Program) -> Vec<bool> {
    let externs = program
        .externs
        .iter()
        .map(|e| {
            let s = e.symbol.as_str();
            let safe = e.noreturn
                || NO_FREE.contains(&s)
                || super::READ_ONLY.contains(&s)
                || s.starts_with("velt_simd_")
                || s.starts_with("velt_atomic_");
            !safe
        })
        .collect();
    let n = program.funcs.len();
    // A call of a function that never returns frees nothing its caller uses after it, only
    // what it is handed (`Frees::may_free`). One imported from another unit (no body here) may
    // return and do anything.
    let returns: Vec<bool> = (program.funcs.iter())
        .map(|f| {
            f.blocks.is_empty()
                || f.blocks
                    .iter()
                    .any(|b| matches!(b.term, Terminator::Return(_)))
        })
        .collect();
    let mut frees = Frees {
        funcs: vec![true; n],
        returns,
        externs,
    };
    let mut changed = vec![false; n];
    for scc in CallGraph::of(program).sccs_bottom_up() {
        for &f in &scc {
            let func = &mut program.funcs[f.0 as usize];
            if !func.blocks.is_empty() && borrow_all(&frees, func) {
                // Drop the freeing paths now unreachable: the summary below must not see them.
                crate::simplify_cfg::run(func);
                changed[f.0 as usize] = true;
            }
        }
        // Least fixed point over the component: optimistic, then whatever calls a freer frees.
        for &f in &scc {
            frees.funcs[f.0 as usize] = program.funcs[f.0 as usize].blocks.is_empty();
        }
        loop {
            let mut grew = false;
            for &f in &scc {
                let i = f.0 as usize;
                if !frees.funcs[i] && frees.frees(&program.funcs[i]) {
                    frees.funcs[i] = true;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
    }
    changed
}

/// Borrow every family of `func` that allows it; returns whether anything changed.
fn borrow_all(frees: &Frees, func: &mut Function) -> bool {
    let usage = Usage::of(func);
    let defs = single_defs(func, &usage);
    let cx = Cx {
        frees,
        func,
        usage: &usage,
        defs: &defs,
    };
    let mut claimed: FxHashSet<Local> = FxHashSet::default();
    let mut found: Vec<(Family, FxHashSet<Point>)> = vec![];
    for (bi, block) in cx.func.blocks.iter().enumerate() {
        for (si, s) in block.stmts.iter().enumerate() {
            let Some(family) = cx.increment_root(s).and_then(|r| cx.family(r, (bi, si))) else {
                continue;
            };
            if family.members.iter().any(|m| claimed.contains(m)) {
                continue;
            }
            if let Some(uses) = cx.check(&family) {
                claimed.extend(family.members.iter().copied());
                found.push((family, uses));
            }
        }
    }
    // A family's freeing path may run inside another's live range (the same array shared by a
    // method and by a callee inlined into it): decide them together, dropping those a call
    // still frees under until the rest agree.
    loop {
        let cut = analysis::cut_of(found.iter().map(|(f, _)| f));
        let reach = cx.reachable(&cut);
        let before = found.len();
        found.retain(|(f, uses)| cx.unfreed(f, &cut, &reach, uses));
        if found.len() == before {
            break;
        }
    }
    if found.is_empty() {
        return false;
    }
    let edits: Vec<Edit> = found.iter().flat_map(|(f, _)| f.edits()).collect();
    for e in edits {
        match e {
            Edit::Nop(b, s) => func.blocks[b].stmts[s] = Stmt::Nop,
            Edit::Goto(b, to) => func.blocks[b].term = Terminator::Goto(to),
        }
    }
    true
}

/// A change to the function.
enum Edit {
    Nop(usize, usize),
    Goto(usize, BlockId),
}

/// A program point: block and statement index.
type Point = (usize, usize);

/// One release of a family: the block branching on `count == 1`, the block that decrements,
/// and the decrementing store in it.
#[derive(Clone, Copy)]
struct Release {
    test: usize,
    dec: BlockId,
    store: Point,
}

struct Family {
    /// The register the increment goes through (written once, by a load).
    root: Local,
    members: FxHashSet<Local>,
    /// Count pointers: registers holding a member less 8.
    counts: FxHashSet<Local>,
    /// The increment's store.
    inc: Point,
    releases: Vec<Release>,
    /// Statements that only compute the count changes (the increment's `add`, a release's load,
    /// test and `sub`).
    count_stmts: FxHashSet<Point>,
}

impl Family {
    fn edits(&self) -> Vec<Edit> {
        let mut e = vec![Edit::Nop(self.inc.0, self.inc.1)];
        for r in &self.releases {
            e.push(Edit::Nop(r.store.0, r.store.1));
            e.push(Edit::Goto(r.test, r.dec));
        }
        e
    }

    fn release_at(&self, b: usize) -> Option<&Release> {
        self.releases.iter().find(|r| r.test == b)
    }
}

/// The single whole-local definition of each register written once.
fn single_defs(func: &Function, usage: &Usage) -> FxHashMap<Local, Point> {
    let mut defs = FxHashMap::default();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (si, s) in block.stmts.iter().enumerate() {
            if let Stmt::Assign(d, _) = s {
                if d.proj.is_empty() && usage.is_register(d.local) && usage.get(d.local).defs == 1 {
                    defs.insert(d.local, (bi, si));
                }
            }
        }
    }
    defs
}

struct Cx<'a> {
    frees: &'a Frees,
    func: &'a Function,
    usage: &'a Usage,
    defs: &'a FxHashMap<Local, Point>,
}

/// `(*q as u64)`: the count word behind count pointer `q`.
fn count_word(p: &Place) -> Option<Local> {
    (p.proj == [Proj::Deref(Ty::U64)]).then_some(p.local)
}

fn local_of(op: &Operand) -> Option<Local> {
    match op {
        Operand::Copy(p) if p.proj.is_empty() => Some(p.local),
        _ => None,
    }
}

fn is_int(op: &Operand, v: i128) -> bool {
    matches!(op, Operand::Const(Const::Int(x), _) if *x == v)
}

impl Cx<'_> {
    fn rvalue_at(&self, at: Point) -> Option<&Rvalue> {
        match self.func.blocks.get(at.0)?.stmts.get(at.1)? {
            Stmt::Assign(_, rv) => Some(rv),
            _ => None,
        }
    }

    /// The defining rvalue of register `l` written once.
    fn def_of(&self, l: Local) -> Option<(Point, &Rvalue)> {
        let at = *self.defs.get(&l)?;
        Some((at, self.rvalue_at(at)?))
    }

    /// `q = ptradd r, -8`: `r`.
    fn count_base(&self, q: Local) -> Option<Local> {
        match self.def_of(q)?.1 {
            Rvalue::Binary(BinOp::PtrAdd, base, off) if is_int(off, -8) => local_of(base),
            _ => None,
        }
    }

    /// `(*q as u64) = n` with `n = add (*q as u64), 1` (or the `add` in place), `q` a count
    /// pointer of a register written once: that register.
    fn increment_root(&self, s: &Stmt) -> Option<Local> {
        let Stmt::Assign(dst, rv) = s else {
            return None;
        };
        let q = count_word(dst)?;
        let add = match rv {
            Rvalue::Use(op) => self.def_of(local_of(op)?)?.1,
            rv => rv,
        };
        let Rvalue::Binary(BinOp::Add, Operand::Copy(c), one) = add else {
            return None;
        };
        if count_word(c) != Some(q) || !is_int(one, 1) {
            return None;
        }
        let r = self.count_base(q)?;
        let ok = self.usage.is_register(r)
            && self.usage.get(r).defs == 1
            && (r.0 as usize) >= self.func.params.len()
            && self.func.locals[r.0 as usize].ty == Ty::Ptr;
        ok.then_some(r)
    }
}

mod analysis;
