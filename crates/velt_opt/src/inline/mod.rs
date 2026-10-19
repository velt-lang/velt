//! Inliner: bottom-up over the call graph's SCCs, so each callee is already optimized (and
//! has its own small callees inlined) before its callers consider it. Direct calls are
//! inlined when the callee is
//! - trivial (cost ≤ `TRIVIAL_COST`) — always, it rarely grows code;
//! - internal, called from exactly one site and not address-taken (it is deleted afterwards,
//!   so there is no growth) — up to `ONCE_COST`;
//! - small (cost ≤ `SMALL_COST`) — while the program-wide growth `Budget` lasts.
//!
//! Functions that never return (panic helpers) are never inlined: their calls are cold.
//! Recursive functions (members of a cycle) are never inlined, neither within their cycle
//! (that would not terminate) nor into outside callers (it only duplicates the entry of the
//! recursion). Signatures stay scalar-only because
//! parameters simply become caller locals; aggregates keep travelling by pointer.

mod splice;

use velt_vir::vir::{Callee, FuncId, Function, Program, Stmt, Terminator};

use crate::callgraph::{direct_calls, CallGraph};

/// Callees at most this big are always inlined.
const TRIVIAL_COST: u64 = 8;
/// Callees at most this big are inlined while the growth budget lasts.
const SMALL_COST: u64 = 40;
/// Single-call-site internal callees at most this big are inlined.
const ONCE_COST: u64 = 2_000;
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
}

impl Budget {
    /// Allow the program to double in size, plus `BUDGET_SLACK`.
    pub fn for_program(program: &Program) -> Budget {
        let size: u64 = program.funcs.iter().map(cost).sum();
        Budget {
            remaining: size + BUDGET_SLACK,
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
    let recursive = (0..program.funcs.len())
        .map(|f| sccs[scc_of[f]].len() > 1 || graph.callees[f].contains(&FuncId(f as u32)))
        .collect();
    let mut state = State {
        recursive,
        call_sites: graph.call_sites.clone(),
        address_taken: graph.address_taken,
    };
    let mut changed = false;
    for scc in &sccs {
        for &caller in scc {
            changed |= inline_into(program, caller, &mut state, budget);
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
            if !helpers.get(ci).copied().unwrap_or(false)
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
    /// Live count of direct call sites per function, updated as bodies are copied.
    call_sites: Vec<u32>,
    address_taken: Vec<bool>,
}

/// How a call site gets inlined (decides whether the budget is charged).
enum Decision {
    Free,
    Charged(u64),
}

fn decide(
    state: &State,
    program: &Program,
    callee: FuncId,
    caller_cost: u64,
    budget: &Budget,
) -> Option<Decision> {
    let ci = callee.0 as usize;
    let target = program.funcs.get(ci)?;
    if target.blocks.is_empty() || state.recursive[ci] || never_returns(target) {
        return None;
    }
    let size = cost(target);
    if size <= TRIVIAL_COST {
        return Some(Decision::Free);
    }
    if caller_cost + size > CALLER_LIMIT {
        return None;
    }
    let once = target.linkage == velt_vir::vir::Linkage::Internal
        && state.call_sites[ci] == 1
        && !state.address_taken[ci];
    if once && size <= ONCE_COST {
        return Some(Decision::Free);
    }
    (size <= SMALL_COST && size <= budget.remaining).then_some(Decision::Charged(size))
}

/// Whether no path through `func` returns (a panic helper such as the out-of-bounds report):
/// such calls are cold, and inlining them only bloats the hot code around them.
fn never_returns(func: &Function) -> bool {
    !func
        .blocks
        .iter()
        .any(|b| matches!(b.term, Terminator::Return(_)))
}

/// Inline eligible calls in `caller`'s original blocks (inlined bodies are not rescanned:
/// their callees were already considered when the callee itself was processed).
fn inline_into(
    program: &mut Program,
    caller: FuncId,
    state: &mut State,
    budget: &mut Budget,
) -> bool {
    let fi = caller.0 as usize;
    let original_blocks = program.funcs[fi].blocks.len();
    let mut caller_cost = cost(&program.funcs[fi]);
    let mut changed = false;
    for bi in 0..original_blocks {
        let Terminator::Call {
            callee: Callee::Func(callee),
            args,
            ..
        } = &program.funcs[fi].blocks[bi].term
        else {
            continue;
        };
        let callee = *callee;
        let Some(target) = program.funcs.get(callee.0 as usize) else {
            continue;
        };
        if !splice::arity_matches(args, target) {
            continue;
        }
        let Some(decision) = decide(state, program, callee, caller_cost, budget) else {
            continue;
        };
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
        splice::inline_call(&program.aggs, &mut program.funcs[fi], bi, &body);
        caller_cost += cost(&body);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests;
