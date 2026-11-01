//! Whole-program call graph: direct call edges, call-site counts, functions whose address is
//! taken (`Const::Func` operands or static relocations), and strongly connected components in bottom-up order.

use std::collections::BTreeSet;

use velt_vir::vir::{
    Callee, Const, ExternId, FuncId, Function, Linkage, Operand, Program, Terminator, Ty,
};

use crate::visit::{stmt_operands, term_operands};

/// A function's parameter and return types.
pub(crate) type Signature = (Vec<Ty>, Ty);

/// Signatures of all functions, indexed by `FuncId`.
pub(crate) fn signatures(program: &Program) -> Vec<Signature> {
    program
        .funcs
        .iter()
        .map(|f| (f.params.clone(), f.ret))
        .collect()
}

/// Direct callees of `func`, one entry per call site.
pub(crate) fn direct_calls(func: &Function) -> impl Iterator<Item = FuncId> + '_ {
    func.blocks.iter().filter_map(|b| match b.term {
        Terminator::Call {
            callee: Callee::Func(id),
            ..
        } => Some(id),
        _ => None,
    })
}

/// Functions whose address `func` takes.
pub(crate) fn address_refs(func: &Function) -> Vec<FuncId> {
    let mut out = Vec::new();
    let mut visit = |op: &Operand| {
        if let Operand::Const(Const::Func(id), _) = op {
            out.push(*id);
        }
    };
    for b in &func.blocks {
        for s in &b.stmts {
            stmt_operands(s, &mut visit);
        }
        term_operands(&b.term, &mut visit);
    }
    out
}

/// Functions whose address is stored in a static (vtable slots and the like).
pub(crate) fn static_refs(program: &Program) -> impl Iterator<Item = FuncId> + '_ {
    program
        .statics
        .iter()
        .flat_map(|s| &s.relocs)
        .filter_map(|(_, target)| match target {
            Const::Func(id) => Some(*id),
            _ => None,
        })
}

/// The externs `program` can reach from its exported functions (its entry): called, taken as an
/// address (`Const::Extern`), or stored in a static that reachable code refers to, following
/// direct calls, function addresses and the functions statics hold (vtable slots). Functions
/// whose symbol `skip` accepts are not followed (#1855: a test harness is also the test runner,
/// and what the runner reaches says nothing about the tests).
pub fn used_externs(program: &Program, skip: impl Fn(&str) -> bool) -> BTreeSet<ExternId> {
    let mut externs = BTreeSet::new();
    let mut seen = vec![false; program.funcs.len()];
    let mut seen_statics = vec![false; program.statics.len()];
    let mut todo: Vec<FuncId> = Vec::new();
    let visit_func = |id: FuncId, seen: &mut Vec<bool>, todo: &mut Vec<FuncId>| {
        let at = id.0 as usize;
        if !seen[at] && !skip(&program.funcs[at].symbol) {
            seen[at] = true;
            todo.push(id);
        }
    };
    for (i, f) in program.funcs.iter().enumerate() {
        if f.linkage == Linkage::Export {
            visit_func(FuncId(i as u32), &mut seen, &mut todo);
        }
    }
    while let Some(id) = todo.pop() {
        let func = program.func(id);
        let mut consts: Vec<Const> = Vec::new();
        let mut callees: Vec<Const> = Vec::new();
        let mut collect = |op: &Operand| {
            if let Operand::Const(c, _) = op {
                consts.push(c.clone());
            }
        };
        for b in &func.blocks {
            for s in &b.stmts {
                stmt_operands(s, &mut collect);
            }
            term_operands(&b.term, &mut collect);
            if let Terminator::Call { callee, .. } = &b.term {
                match callee {
                    Callee::Func(f) => callees.push(Const::Func(*f)),
                    Callee::Extern(e) => callees.push(Const::Extern(*e)),
                    Callee::Ptr { .. } => {}
                }
            }
        }
        consts.extend(callees);
        // Statics reached, then the functions and externs they hold (and statics they point to).
        while let Some(c) = consts.pop() {
            match c {
                Const::Func(f) => visit_func(f, &mut seen, &mut todo),
                Const::Extern(e) => {
                    externs.insert(e);
                }
                Const::Static(st) => {
                    let at = st.0 as usize;
                    if let Some(data) = program.statics.get(at).filter(|_| !seen_statics[at]) {
                        seen_statics[at] = true;
                        consts.extend(data.relocs.iter().map(|(_, target)| target.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    externs
}

/// Call graph facts for the whole program.
pub(crate) struct CallGraph {
    /// Deduplicated direct callees per function.
    pub callees: Vec<Vec<FuncId>>,
    /// Number of direct call sites targeting each function.
    pub call_sites: Vec<u32>,
    /// Whether the function's address is taken anywhere (it may be called indirectly).
    pub address_taken: Vec<bool>,
}

impl CallGraph {
    /// Build the graph.
    pub fn of(program: &Program) -> CallGraph {
        let n = program.funcs.len();
        let mut callees = vec![Vec::new(); n];
        let mut call_sites = vec![0; n];
        let mut address_taken = vec![false; n];
        for (i, f) in program.funcs.iter().enumerate() {
            for id in direct_calls(f).filter(|id| (id.0 as usize) < n) {
                call_sites[id.0 as usize] += 1;
                callees[i].push(id);
            }
            callees[i].sort();
            callees[i].dedup();
            for id in address_refs(f).into_iter().filter(|id| (id.0 as usize) < n) {
                address_taken[id.0 as usize] = true;
            }
        }
        for id in static_refs(program).filter(|id| (id.0 as usize) < n) {
            address_taken[id.0 as usize] = true;
        }
        CallGraph {
            callees,
            call_sites,
            address_taken,
        }
    }

    /// Strongly connected components, callees before callers (Tarjan's order), computed
    /// iteratively so deep call chains cannot overflow the compiler's stack.
    pub fn sccs_bottom_up(&self) -> Vec<Vec<FuncId>> {
        let edges: Vec<Vec<usize>> = self
            .callees
            .iter()
            .map(|cs| cs.iter().map(|c| c.0 as usize).collect())
            .collect();
        crate::scc::sccs(&edges)
            .into_iter()
            .map(|scc| scc.into_iter().map(|v| FuncId(v as u32)).collect())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{Callee, Rvalue, Ty};

    /// f0 → f1 ⇄ f2, f2 → f3; f0 takes f3's address.
    fn program() -> Program {
        let mut pb = ProgramBuilder::new();
        let ids: Vec<FuncId> = (0..4).map(|_| pb.reserve()).collect();
        let calls: [&[usize]; 4] = [&[1], &[2], &[1, 3], &[]];
        for (i, targets) in calls.iter().enumerate() {
            let mut fb = FuncBuilder::internal(&format!("f{i}"), &[], Ty::Unit);
            let mut b = fb.block();
            if i == 0 {
                let p = fb.local(Ty::Ptr);
                fb.assign(
                    b,
                    p,
                    Rvalue::Use(Operand::Const(Const::Func(ids[3]), Ty::Ptr)),
                );
            }
            for &t in *targets {
                b = fb.call(b, Callee::Func(ids[t]), vec![], None);
            }
            fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
            pb.set(ids[i], fb.finish());
        }
        pb.finish()
    }

    #[test]
    fn sccs_are_bottom_up() {
        let g = CallGraph::of(&program());
        let sccs = g.sccs_bottom_up();
        let pos = |id: u32| sccs.iter().position(|s| s.contains(&FuncId(id))).unwrap();
        assert_eq!(sccs.len(), 3);
        assert_eq!(pos(1), pos(2));
        assert!(pos(3) < pos(1));
        assert!(pos(1) < pos(0));
    }

    /// `main` calls `pure`, `runner` and `ext_a`, takes `vt`'s address (a static holding `slot`,
    /// which calls `ext_b`); `runner` calls `ext_c`; `unused` calls `ext_d` and nothing calls it.
    #[test]
    fn used_externs_follow_calls_addresses_and_statics_but_not_skipped_functions() {
        let mut pb = ProgramBuilder::new();
        let [a, b, c, d] =
            ["ext_a", "ext_b", "ext_c", "ext_d"].map(|n| pb.ext(n, &[], Ty::Unit, false));
        let calling = |name: &str, callees: &[Callee], export: bool| {
            let mut fb = match export {
                true => FuncBuilder::export(name, &[], Ty::Unit),
                false => FuncBuilder::internal(name, &[], Ty::Unit),
            };
            let mut blk = fb.block();
            for callee in callees {
                blk = fb.call(blk, callee.clone(), vec![], None);
            }
            fb.ret(blk, Operand::Const(Const::Unit, Ty::Unit));
            fb.finish()
        };
        let slot = pb.add(calling("slot", &[Callee::Extern(b)], false));
        let vt = pb.stat_with(&[0; 8], 8, vec![(0, Const::Func(slot))]);
        let runner = pb.add(calling("runner", &[Callee::Extern(c)], false));
        pb.add(calling("unused", &[Callee::Extern(d)], false));
        let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
        let mut blk = fb.block();
        let p = fb.local(Ty::Ptr);
        fb.assign(
            blk,
            p,
            Rvalue::Use(Operand::Const(Const::Static(vt), Ty::Ptr)),
        );
        blk = fb.call(blk, Callee::Func(runner), vec![], None);
        blk = fb.call(blk, Callee::Extern(a), vec![], None);
        fb.ret(blk, Operand::Const(Const::Unit, Ty::Unit));
        pb.add(fb.finish());
        let program = pb.finish();
        let all = used_externs(&program, |_| false);
        assert_eq!(all, BTreeSet::from([a, b, c]));
        let without_runner = used_externs(&program, |s| s == "runner");
        assert_eq!(without_runner, BTreeSet::from([a, b]));
    }

    #[test]
    fn counts_sites_and_address_taken() {
        let g = CallGraph::of(&program());
        assert_eq!(g.call_sites, vec![0, 2, 1, 1]);
        assert_eq!(g.address_taken, vec![false, false, false, true]);
    }
}
