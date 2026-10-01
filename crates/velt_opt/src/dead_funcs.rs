//! Unused function removal: internal functions that are neither reachable (by direct calls or
//! address references) from an exported function nor stored in a static (vtable slots, kept
//! conservatively even if the static is unused) are deleted, and `FuncId`s renumbered.
//! Inlining leaves many of these behind (called-once helpers, fully inlined getters).

use velt_vir::vir::{Callee, Const, FuncId, Linkage, Operand, Program, Terminator};

use crate::callgraph::{address_refs, direct_calls, static_refs};
use crate::visit::{stmt_operands_mut, term_operands_mut};

/// Remove unreachable internal functions; returns whether any were removed.
pub(crate) fn run(program: &mut Program) -> bool {
    let live = live_functions(program);
    if live.iter().all(|&l| l) {
        return false;
    }
    let mut remap = vec![None; live.len()];
    let mut next = 0u32;
    for (i, &l) in live.iter().enumerate() {
        if l {
            remap[i] = Some(FuncId(next));
            next += 1;
        }
    }
    let mut index = 0;
    program.funcs.retain(|_| {
        index += 1;
        live[index - 1]
    });
    let new_id =
        |id: FuncId| remap[id.0 as usize].expect("ICE: live function references a dead one");
    for func in &mut program.funcs {
        for block in &mut func.blocks {
            for s in &mut block.stmts {
                stmt_operands_mut(s, &mut |op| renumber_operand(op, &new_id));
            }
            term_operands_mut(&mut block.term, &mut |op| renumber_operand(op, &new_id));
            if let Terminator::Call {
                callee: Callee::Func(id),
                ..
            } = &mut block.term
            {
                *id = new_id(*id);
            }
        }
    }
    for s in &mut program.statics {
        for (_, target) in &mut s.relocs {
            if let Const::Func(id) = target {
                *id = new_id(*id);
            }
        }
    }
    true
}

fn renumber_operand(op: &mut Operand, new_id: &impl Fn(FuncId) -> FuncId) {
    if let Operand::Const(Const::Func(id), _) = op {
        *id = new_id(*id);
    }
}

/// Mark functions reachable from exported roots.
fn live_functions(program: &Program) -> Vec<bool> {
    let n = program.funcs.len();
    let mut live = vec![false; n];
    let mut work: Vec<usize> = (0..n)
        .filter(|&i| program.funcs[i].linkage == Linkage::Export)
        .chain(
            static_refs(program)
                .map(|id| id.0 as usize)
                .filter(|&i| i < n),
        )
        .collect();
    work.sort_unstable();
    work.dedup();
    for &i in &work {
        live[i] = true;
    }
    while let Some(i) = work.pop() {
        let f = &program.funcs[i];
        for id in direct_calls(f).chain(address_refs(f)) {
            let j = id.0 as usize;
            if j < n && !live[j] {
                live[j] = true;
                work.push(j);
            }
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use crate::testkit::validate::assert_valid;
    use velt_vir::vir::{Rvalue, Ty};

    #[test]
    fn removes_unreachable_and_renumbers() {
        let mut pb = ProgramBuilder::new();
        let unit = || Operand::Const(Const::Unit, Ty::Unit);
        // f0: dead. f1: address-taken by main. f2: called by main. f3: main.
        for name in ["dead", "by_addr", "called"] {
            let mut fb = FuncBuilder::internal(name, &[], Ty::Unit);
            let b = fb.block();
            fb.ret(b, unit());
            pb.add(fb.finish());
        }
        let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
        let p = fb.local(Ty::Ptr);
        let b = fb.block();
        fb.assign(
            b,
            p,
            Rvalue::Use(Operand::Const(Const::Func(FuncId(1)), Ty::Ptr)),
        );
        let b = fb.call(b, Callee::Func(FuncId(2)), vec![], None);
        fb.ret(b, unit());
        pb.add(fb.finish());
        let mut p = pb.finish();
        assert!(run(&mut p));
        let names: Vec<_> = p.funcs.iter().map(|f| f.symbol.as_str()).collect();
        assert_eq!(names, ["by_addr", "called", "main"]);
        assert_valid(&p);
        let main = &p.funcs[2];
        assert!(matches!(
            main.blocks[0].stmts[0],
            velt_vir::vir::Stmt::Assign(_, Rvalue::Use(Operand::Const(Const::Func(FuncId(0)), _)))
        ));
        assert!(matches!(
            main.blocks[0].term,
            Terminator::Call {
                callee: Callee::Func(FuncId(1)),
                ..
            }
        ));
        assert!(!run(&mut p));
    }

    #[test]
    fn functions_in_static_relocations_are_live() {
        let unit = || Operand::Const(Const::Unit, Ty::Unit);
        let mut pb = ProgramBuilder::new();
        for name in ["dead", "in_vtable"] {
            let mut fb = FuncBuilder::internal(name, &[], Ty::Unit);
            let b = fb.block();
            fb.ret(b, unit());
            pb.add(fb.finish());
        }
        let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
        let b = fb.block();
        fb.ret(b, unit());
        pb.add(fb.finish());
        pb.stat_with(&[0; 8], 8, vec![(0, Const::Func(FuncId(1)))]);
        let mut p = pb.finish();
        assert!(run(&mut p));
        let names: Vec<_> = p.funcs.iter().map(|f| f.symbol.as_str()).collect();
        assert_eq!(names, ["in_vtable", "main"]);
        assert_eq!(p.statics[0].relocs, [(0, Const::Func(FuncId(0)))]);
        assert_valid(&p);
    }
}
