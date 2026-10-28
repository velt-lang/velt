//! Inliner: bottom-up over the call graph's SCCs, so each callee is already optimized (and
//! has its own small callees inlined) before its callers consider it. Direct calls are
//! inlined when the callee is
//! - trivial (cost ≤ `TRIVIAL_COST`) — always, it rarely grows code;
//! - internal, called from exactly one site and not address-taken (it is deleted afterwards,
//!   so there is no growth) — up to `ONCE_COST`;
//! - small (cost ≤ `SMALL_COST`, or ≤ `LEAF_COST` for a leaf, which calls nothing but
//!   functions that never return: `readVarint`, a byte reader with bounds checks) — while the
//!   program-wide growth `Budget` lasts;
//! - called from a loop of the caller (cost ≤ `LOOP_COST`) — while the budget lasts: the hot
//!   paths of a program are its loops, and a call per iteration costs the call and whatever the
//!   callee keeps in memory across it. (The standard library's caller-tracking functions get an
//!   instance per call site, track_caller.rs in velt_vir, so they meet the single-site rule;
//!   this rule gives every program's loops the same, its own modules' functions included.)
//!
//! Functions that never return normally (panic helpers, and `never` functions that throw) are
//! never inlined: their calls are cold. A gated
//! function (`Function::target`) is never inlined into a function of another target, or into an
//! ordinary one; an ordinary function may be inlined into a gated one.
//! Recursive functions (members of a cycle) are never inlined within their cycle (that would
//! not terminate). Into a loop of an outside caller, a small one (cost ≤ `RECURSIVE_COST`) is
//! inlined once: the calls back into its cycle that its body brings along stay calls, in that
//! round (the blocks an inlined body adds remember the cycles they came from) and in later
//! rounds (a caller remembers the cycles it has a copy of, `Budget::cycles`). So a mutual
//! recursion that is rare in practice (`Cursor.int` converting a REAL through `real`, which
//! converts the other types through `int`) does not keep its hot entry out of the loops that
//! call it, without unrolling the recursion. A function that calls itself recurses in earnest
//! (divide and conquer, a tree walk: a copy of its entry saves one call of many) and stays a
//! call, as does recursive glue (drop glue of a type that can hold itself), which runs where a
//! last reference goes away. Elsewhere, and past that size, a copy would only add to a body
//! that stays for the calls from the cycle.
//!
//! Signatures stay scalar-only because parameters simply become caller locals; aggregates keep
//! travelling by pointer.

mod splice;

use rustc_hash::FxHashMap;
use velt_vir::vir::{Callee, FuncId, Function, Program, Stmt, Terminator};

use crate::callgraph::{direct_calls, CallGraph};
use crate::scc::on_cycle;
use crate::visit::successors;

/// Callees at most this big are always inlined.
const TRIVIAL_COST: u64 = 8;
/// Callees at most this big are inlined while the growth budget lasts.
const SMALL_COST: u64 = 40;
/// Leaf callees (no calls but of functions that never return) at most this big count as small:
/// the call is all they add, and inlined, what they compute (a decoded value and its length
/// returned through memory) becomes locals of the caller.
const LEAF_COST: u64 = 120;
/// Single-call-site internal callees at most this big are inlined.
const ONCE_COST: u64 = 2_000;
/// Callees called from a loop at most this big are inlined while the growth budget lasts.
const LOOP_COST: u64 = 1_000;
/// Recursive callees at most this big are inlined into loops of outside callers (while the
/// growth budget lasts).
const RECURSIVE_COST: u64 = 200;
/// Callers are not grown past this size (keeps compile time and register pressure sane).
const CALLER_LIMIT: u64 = 10_000;
/// Growth allowed on top of the program's size: the program may at most double, plus this.
const BUDGET_SLACK: u64 = 2_000;

/// Size estimate: statements plus terminators.
pub(crate) fn cost(func: &Function) -> u64 {
    let stmts = func
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| !matches!(s, Stmt::Nop))
        .count();
    (stmts + func.blocks.len()) as u64
}

/// Program-wide code growth allowance for inlining, shared by all pipeline rounds.
pub(crate) struct Budget {
    remaining: u64,
    /// Per caller, recursive functions it has a copy of (inlined into it, directly or within
    /// another callee's body): their cycles are not inlined into it again (module docs).
    cycles: FxHashMap<FuncId, Vec<FuncId>>,
}

impl Budget {
    /// Allow the program to double in size, plus `BUDGET_SLACK`.
    pub fn for_program(program: &Program) -> Budget {
        let size: u64 = program.funcs.iter().map(cost).sum();
        Budget {
            remaining: size + BUDGET_SLACK,
            cycles: FxHashMap::default(),
        }
    }
}

/// Inline call sites across the program; returns whether anything was inlined.
pub(crate) fn run(program: &mut Program, budget: &mut Budget) -> bool {
    let graph = CallGraph::of(program);
    let mut scc_of = vec![0usize; program.funcs.len()];
    let sccs = graph.sccs_bottom_up();
    for (i, scc) in sccs.iter().enumerate() {
        for f in scc {
            scc_of[f.0 as usize] = i;
        }
    }
    let calls_itself: Vec<bool> = (0..program.funcs.len())
        .map(|f| graph.callees[f].contains(&FuncId(f as u32)))
        .collect();
    let recursive = (0..program.funcs.len())
        .map(|f| sccs[scc_of[f]].len() > 1 || calls_itself[f])
        .collect();
    let mut state = State {
        recursive,
        calls_itself,
        scc_of,
        call_sites: graph.call_sites.clone(),
        address_taken: graph.address_taken,
    };
    let mut changed = false;
    for scc in &sccs {
        for &caller in scc {
            if inline_into(program, caller, &mut state, budget) {
                changed = true;
                // Its callees' reads of its `frozen` params are its own now, while it still
                // has the params: read them once before it is inlined into its own callers.
                let f = &mut program.funcs[caller.0 as usize];
                crate::frozen::run(&program.aggs, f);
                // Checks of inlined callees (a node read through helpers) and the caller's own
                // range check are in one function now, while its frozen params still name the
                // array lengths.
                if f.param_attrs.iter().any(|a| a.frozen) {
                    crate::bounds::run(&program.aggs, f);
                }
            }
        }
    }
    changed
}

/// Inline every direct call to a function marked in `helpers` (debug builds: the compiler's
/// small helpers only, so user code keeps its calls and stays debuggable). Helpers must not
/// call each other.
pub(crate) fn run_helpers(program: &mut Program, helpers: &[bool]) -> bool {
    let mut changed = false;
    for fi in 0..program.funcs.len() {
        if helpers[fi] {
            continue;
        }
        let original_blocks = program.funcs[fi].blocks.len();
        for bi in 0..original_blocks {
            let Terminator::Call {
                callee: Callee::Func(callee),
                args,
                ..
            } = &program.funcs[fi].blocks[bi].term
            else {
                continue;
            };
            let ci = callee.0 as usize;
            // A declaration (an `Import` without a body) stays a call.
            if !helpers.get(ci).copied().unwrap_or(false)
                || program.funcs[ci].blocks.is_empty()
                || !splice::arity_matches(args, &program.funcs[ci])
            {
                continue;
            }
            let body = program.funcs[ci].clone();
            splice::inline_call(&program.aggs, &mut program.funcs[fi], bi, &body);
            changed = true;
        }
    }
    changed
}

struct State {
    recursive: Vec<bool>,
    /// Per function, whether it calls itself directly.
    calls_itself: Vec<bool>,
    /// Per function, its SCC (this round's numbering).
    scc_of: Vec<usize>,
    /// Live count of direct call sites per function, updated as bodies are copied.
    call_sites: Vec<u32>,
    address_taken: Vec<bool>,
}

/// How a call site gets inlined (decides whether the budget is charged).
enum Decision {
    Free,
    Charged(u64),
}

/// Where a call site is, for [`decide`].
struct Site<'a> {
    caller: FuncId,
    caller_cost: u64,
    in_loop: bool,
    /// Recursive functions whose inlined bodies the call came with (this round).
    from: &'a [FuncId],
}

fn decide(
    state: &State,
    program: &Program,
    callee: FuncId,
    site: &Site,
    budget: &Budget,
) -> Option<Decision> {
    let ci = callee.0 as usize;
    let target = program.funcs.get(ci)?;
    if target.blocks.is_empty() || never_returns(target) {
        return None;
    }
    let recursive = state.recursive[ci];
    if recursive {
        // One that calls itself (divide and conquer, a tree walk) recurses in earnest: a copy of
        // its entry saves one call of many, so it stays a call. So does recursive glue (`_G…`:
        // drop glue, recursive through the types it frees), which runs where a last reference
        // goes away.
        if state.calls_itself[ci] || target.symbol.starts_with("_G") {
            return None;
        }
        // Not within its cycle, and once per caller (module docs).
        let cycle = state.scc_of[ci];
        let same = |f: &FuncId| state.scc_of.get(f.0 as usize) == Some(&cycle);
        let had = budget
            .cycles
            .get(&site.caller)
            .is_some_and(|c| c.iter().any(same));
        if state.scc_of[site.caller.0 as usize] == cycle || had || site.from.iter().any(same) {
            return None;
        }
    }
    let size = cost(target);
    if size <= TRIVIAL_COST {
        return Some(Decision::Free);
    }
    if site.caller_cost + size > CALLER_LIMIT {
        return None;
    }
    let once = target.linkage == velt_vir::vir::Linkage::Internal
        && !recursive
        && state.call_sites[ci] == 1
        && !state.address_taken[ci];
    if once && size <= ONCE_COST {
        return Some(Decision::Free);
    }
    if recursive {
        // A copy of a recursive function adds to its body, which stays: only a small entry
        // called from a loop is worth one (module docs).
        let ok = site.in_loop && size <= RECURSIVE_COST && size <= budget.remaining;
        return ok.then_some(Decision::Charged(size));
    }
    let limit = if site.in_loop {
        LOOP_COST
    } else if size > SMALL_COST && is_leaf(program, target) {
        LEAF_COST
    } else {
        SMALL_COST
    };
    (size <= limit && size <= budget.remaining).then_some(Decision::Charged(size))
}

/// Whether `func` calls nothing but functions that never return (its checks' reports).
fn is_leaf(program: &Program, func: &Function) -> bool {
    func.blocks.iter().all(|b| match &b.term {
        Terminator::Call { callee, .. } => match callee {
            Callee::Func(f) => program.funcs.get(f.0 as usize).is_some_and(never_returns),
            Callee::Extern(e) => program
                .externs
                .get(e.0 as usize)
                .is_some_and(|e| e.noreturn),
            Callee::Ptr { .. } => false,
        },
        _ => true,
    })
}

/// Whether no call of `func` returns normally: no path through it returns (a panic helper such
/// as the out-of-bounds report), or it is declared `never` and returns only its error (a throw
/// helper such as `corrupt(what): never`, `Function::cold`). Such calls are cold, and inlining
/// them only bloats the hot code around them.
pub(crate) fn never_returns(func: &Function) -> bool {
    // A function imported from another unit has no body here: it may return.
    func.cold
        || (!func.blocks.is_empty()
            && !func
                .blocks
                .iter()
                .any(|b| matches!(b.term, Terminator::Return(_))))
}

/// Inline eligible calls in `caller`'s original blocks. Inlined bodies are not rescanned
/// (their callees were already considered when the callee itself was processed), except one
/// inlined into a loop: its calls are in the loop now, which the callee did not know.
fn inline_into(
    program: &mut Program,
    caller: FuncId,
    state: &mut State,
    budget: &mut Budget,
) -> bool {
    let fi = caller.0 as usize;
    let mut caller_cost = cost(&program.funcs[fi]);
    // Per block: whether it is in a loop, and whether to scan it. Inlining appends the
    // callee's blocks, so the blocks before them keep their numbers.
    let mut in_loop = loop_blocks(&program.funcs[fi]);
    let mut scan = vec![true; in_loop.len()];
    // Per block: the recursive functions whose inlined bodies it came with.
    let mut from: Vec<Vec<FuncId>> = vec![vec![]; in_loop.len()];
    // The recursive functions this caller gets copies of in this round.
    let mut copied: Vec<FuncId> = vec![];
    let mut changed = false;
    let mut bi = 0;
    while bi < program.funcs[fi].blocks.len() {
        let here = bi;
        bi += 1;
        if !scan[here] {
            continue;
        }
        let Terminator::Call {
            callee: Callee::Func(callee),
            args,
            ..
        } = &program.funcs[fi].blocks[here].term
        else {
            continue;
        };
        let callee = *callee;
        let Some(target) = program.funcs.get(callee.0 as usize) else {
            continue;
        };
        // A gated function (`Function::target`, #1330) is compiled for its CPU extension and
        // runs only where the CPU has it: its body must not move into a function compiled
        // without (or for another) extension. An ordinary body may move into it: there it only
        // runs where the CPU has the extension.
        let gated_elsewhere = target.target.is_some() && target.target != program.funcs[fi].target;
        if !splice::arity_matches(args, target) || gated_elsewhere {
            continue;
        }
        let site = Site {
            caller,
            caller_cost,
            in_loop: in_loop[here],
            from: &from[here],
        };
        let Some(decision) = decide(state, program, callee, &site, budget) else {
            continue;
        };
        // What the callee's blocks come with: the call's own provenance, the callee if it is
        // recursive, and the recursive functions the callee has copies of.
        let mut brings = from[here].clone();
        if state.recursive[callee.0 as usize] {
            brings.push(callee);
        }
        if let Some(c) = budget.cycles.get(&callee) {
            brings.extend(c.iter().copied());
        }
        copied.extend(brings.iter().copied());
        if let Decision::Charged(size) = decision {
            budget.remaining -= size;
        }
        let body = program.funcs[callee.0 as usize].clone();
        state.call_sites[callee.0 as usize] -= 1;
        for nested in direct_calls(&body) {
            if let Some(n) = state.call_sites.get_mut(nested.0 as usize) {
                *n += 1;
            }
        }
        splice::inline_call(&program.aggs, &mut program.funcs[fi], here, &body);
        let grown = program.funcs[fi].blocks.len();
        in_loop.resize(grown, in_loop[here]);
        scan.resize(grown, in_loop[here]);
        from.resize(grown, brings);
        caller_cost += cost(&body);
        changed = true;
    }
    if !copied.is_empty() {
        let mine = budget.cycles.entry(caller).or_default();
        for f in copied {
            if !mine.contains(&f) {
                mine.push(f);
            }
        }
    }
    changed
}

/// Per block of `func`: whether it lies on a cycle of the control-flow graph (in a loop).
fn loop_blocks(func: &Function) -> Vec<bool> {
    let edges: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| successors(&b.term).iter().map(|s| s.0 as usize).collect())
        .collect();
    on_cycle(&edges)
}

#[cfg(test)]
mod tests;
