//! What narrowing the elements of an array costs: the conversions between floats and integers
//! a function makes, weighted by the loop depth of their blocks.

use velt_vir::vir::{Callee, Function, Operand, Rvalue, Stmt, Terminator};

use super::super::Env;

/// Does `rv` round a float or take its ToInt32 (`Rvalue::Intrinsic`, as `float_call` for the
/// call of the runtime function)?
pub(super) fn float_op(rv: &Rvalue) -> bool {
    super::super::math_rvalue(rv)
        .is_some_and(|(op, _)| op.rounding().is_some() || op == velt_vir::math::MathOp::ToInt32)
}

/// Does `callee` round a float, take its ToInt32 or index with it?
pub(super) fn float_call(env: &Env, callee: &Callee) -> bool {
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
pub(super) fn conversions(env: &Env, func: &Function, depth: &[u32]) -> u64 {
    let ty = |op: &Operand| super::super::flow::operand_ty(func, op);
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
            if matches!(s, Stmt::Assign(_, rv) if float_op(rv)) {
                total += w;
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
pub(super) fn loop_depths(func: &Function) -> Vec<u32> {
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
