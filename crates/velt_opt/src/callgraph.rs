//! Whole-program call graph: direct call edges, call-site counts, functions whose address is
//! taken (`Const::Func` operands or static relocations), and strongly connected components in bottom-up order.

use velt_vir::vir::{Callee, Const, FuncId, Function, Operand, Program, Terminator, Ty};

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

    #[test]
    fn counts_sites_and_address_taken() {
        let g = CallGraph::of(&program());
        assert_eq!(g.call_sites, vec![0, 2, 1, 1]);
        assert_eq!(g.address_taken, vec![false, false, false, true]);
    }
}
