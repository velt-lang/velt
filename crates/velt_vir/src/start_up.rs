//! The code that runs before std's start-up check of the CPU knows what the CPU has (rt_abi.md
//! "CPU check"): the check itself, std/lang/hooks/cpu.velt `cpuStart`, which `velt_main` calls
//! first, and every function it reaches by a call or takes the address of. [`mark_baseline`] sets
//! their `Function::baseline`, and backends compile them for the baseline CPU of the architecture
//! whatever `--cpu` says: on a CPU without the build's features the check must still get to print
//! its message, not die of an illegal instruction in its own code.
//!
//! The closure is over the VIR as it is when it runs: the driver marks after lowering, so the std
//! cache serves none of these functions compiled for `--cpu`, and again right before code
//! generation, after `velt_opt` has inlined into them. A function also called after the check
//! (none in std today) is compiled for the baseline too, which costs it only speed.

use crate::vir::{Callee, Const, Operand, Program, Rvalue, Stmt, Terminator};

/// The start-up check's entry, by its demangled name (`crate::mangle::demangle`).
pub const ENTRY: &str = velt_common::std_item!(lang_hooks_cpu, "cpuStart");

/// Mark every function the start-up check reaches as `baseline`. Idempotent; a program without
/// the check (a test's, or one lowered without the hook) is left as it is.
pub fn mark_baseline(program: &mut Program) {
    let mut todo: Vec<usize> = (0..program.funcs.len())
        .filter(|&i| crate::mangle::demangle(&program.funcs[i].symbol) == ENTRY)
        .collect();
    let mut seen = vec![false; program.funcs.len()];
    while let Some(i) = todo.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        program.funcs[i].baseline = true;
        each_callee(program, i, &mut |j| {
            if !seen[j] {
                todo.push(j);
            }
        });
    }
}

/// Each function `program.funcs[i]` calls directly or takes the address of.
pub fn each_callee(program: &Program, i: usize, each: &mut dyn FnMut(usize)) {
    let operand = |op: &Operand, each: &mut dyn FnMut(usize)| {
        if let Operand::Const(Const::Func(f), _) = op {
            each(f.0 as usize);
        }
    };
    for b in &program.funcs[i].blocks {
        for s in &b.stmts {
            match s {
                Stmt::Assign(_, rv) => match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => operand(a, each),
                    Rvalue::Binary(_, a, c) => {
                        operand(a, each);
                        operand(c, each);
                    }
                    Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => {
                        ops.iter().for_each(|o| operand(o, each))
                    }
                    Rvalue::AddrOf(_) => {}
                },
                Stmt::MemCopy { dst, src, .. } => {
                    operand(dst, each);
                    operand(src, each);
                }
                Stmt::MemCopyDyn { dst, src, len, .. } => {
                    for o in [dst, src, len] {
                        operand(o, each);
                    }
                }
                Stmt::MemSet { dst, byte, len } => {
                    for o in [dst, byte, len] {
                        operand(o, each);
                    }
                }
                Stmt::Nop => {}
            }
        }
        match &b.term {
            Terminator::Branch { cond: o, .. }
            | Terminator::Switch { value: o, .. }
            | Terminator::Return(o) => operand(o, each),
            Terminator::Call { callee, args, .. } => {
                match callee {
                    Callee::Func(f) => each(f.0 as usize),
                    Callee::Ptr { target, .. } => operand(target, each),
                    Callee::Extern(_) => {}
                }
                args.iter().for_each(|o| operand(o, each));
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vir::{BasicBlock, BlockId, FuncId, Function, Linkage, Ty};

    fn func(name: &str, calls: &[u32], address_of: Option<u32>) -> Function {
        let mut f = Function::new(
            crate::mangle::mangle(name),
            vec![],
            Ty::Unit,
            Linkage::Internal,
        );
        let mut blocks: Vec<BasicBlock> = (calls.iter().enumerate())
            .map(|(i, &c)| BasicBlock {
                stmts: vec![],
                term: Terminator::Call {
                    callee: Callee::Func(FuncId(c)),
                    args: vec![],
                    dest: None,
                    next: BlockId(i as u32 + 1),
                    unwind: None,
                },
            })
            .collect();
        let ret = match address_of {
            Some(g) => Operand::Const(Const::Func(FuncId(g)), Ty::Ptr),
            None => Operand::Const(Const::Unit, Ty::Unit),
        };
        blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Return(ret),
        });
        f.blocks = blocks;
        f
    }

    /// The check, what it calls (directly or through what it calls) and what it takes the address
    /// of are start-up code; `velt_main`, which calls the check, and code only it reaches are not.
    #[test]
    fn the_check_and_what_it_reaches_are_marked() {
        let mut p = Program {
            aggs: vec![],
            funcs: vec![
                func("velt_main", &[1, 4], None),
                func(ENTRY, &[2], Some(3)),
                func("std/lang/cpu/start::startWith", &[1], None),
                func("std/lang/cpu/check::nameOf", &[], None),
                func("user", &[2], None),
            ],
            externs: vec![],
            statics: vec![],
            files: vec![],
            debug_types: vec![],
        };
        mark_baseline(&mut p);
        let marked: Vec<bool> = p.funcs.iter().map(|f| f.baseline).collect();
        assert_eq!(marked, [false, true, true, true, false]);
        // A program without the check has none.
        let mut q = Program {
            funcs: vec![func("main", &[1], None), func("helper", &[], None)],
            ..p
        };
        q.funcs.iter_mut().for_each(|f| f.baseline = false);
        mark_baseline(&mut q);
        assert!(q.funcs.iter().all(|f| !f.baseline));
    }
}
