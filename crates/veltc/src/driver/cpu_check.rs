//! The start-up CPU check (rt_abi.md "CPU check"): a program built for more than the baseline
//! x86-64 CPU calls `velt_rt_check_cpu(<features>)` first thing in `velt_main`, so on a CPU without
//! them it exits with a message naming what is missing instead of dying of an illegal
//! instruction. Added to the VIR right before code generation, so both backends emit it and the
//! `velt dev` JIT (which runs on the machine it compiles for) does not.

use velt_vir::vir::{
    BasicBlock, BlockId, Callee, Const, ExternFn, ExternId, Operand, Program, Terminator, Ty,
};

/// The runtime function that checks the CPU.
const CHECK: &str = "velt_rt_check_cpu";

/// Make `velt_main` check that the CPU has the features `required` (a mask over
/// `velt_codegen_cl::X86_FEATURES`) before it runs anything else; nothing when `required` is 0.
/// The entry block moves to the end, and the new entry calls the check and goes on there.
pub(super) fn insert(program: &mut Program, required: u32) {
    if required == 0 {
        return;
    }
    let Some(main) = program.funcs.iter().position(|f| f.symbol == "velt_main") else {
        return;
    };
    let ext = ExternId(program.externs.len() as u32);
    program.externs.push(ExternFn {
        symbol: CHECK.into(),
        params: vec![Ty::U32],
        ret: Ty::Unit,
        noreturn: false,
    });
    let f = &mut program.funcs[main];
    let moved = BlockId(f.blocks.len() as u32);
    let check = BasicBlock {
        stmts: vec![],
        term: Terminator::Call {
            callee: Callee::Extern(ext),
            args: vec![Operand::Const(Const::Int(required.into()), Ty::U32)],
            dest: None,
            next: moved,
        },
    };
    let Some(entry) = f.blocks.first_mut() else {
        return;
    };
    let entry = std::mem::replace(entry, check);
    f.blocks.push(entry);
    // The check gets the location of the entry's first statement (invariant 8).
    if let Some(first) = f.locs.first_mut() {
        let loc = first.iter().flatten().next().copied();
        let moved_locs = std::mem::replace(first, vec![loc]);
        f.locs.push(moved_locs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{compile, BuildOptions, Session};

    fn program() -> Program {
        let opts = BuildOptions {
            input: "main.vlt".into(),
            root_source: Some("function main() {\n  console.log(\"hi\");\n}\n".into()),
            ..Default::default()
        };
        let mut sess = Session::new();
        compile(&mut sess, &opts).unwrap_or_else(|_| panic!("{}", sess.render_diagnostics()))
    }

    #[test]
    fn main_checks_the_cpu_first() {
        let mut p = program();
        let main = p
            .funcs
            .iter()
            .position(|f| f.symbol == "velt_main")
            .unwrap();
        let blocks = p.funcs[main].blocks.len();
        insert(&mut p, 0x3f);
        velt_vir::verify(&p).unwrap_or_else(|e| panic!("{e:?}"));
        let ext = ExternId(p.externs.len() as u32 - 1);
        assert_eq!(p.ext(ext).symbol, CHECK);
        let f = &p.funcs[main];
        assert_eq!(f.blocks.len(), blocks + 1);
        match &f.blocks[0].term {
            Terminator::Call {
                callee, args, next, ..
            } => {
                assert_eq!(callee, &Callee::Extern(ext));
                assert_eq!(args, &[Operand::Const(Const::Int(0x3f), Ty::U32)]);
                assert_eq!(*next, BlockId(blocks as u32));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn baseline_code_checks_nothing() {
        let mut p = program();
        let before = (
            p.externs.len(),
            p.funcs.iter().map(|f| f.blocks.len()).sum::<usize>(),
        );
        insert(&mut p, 0);
        let after = (
            p.externs.len(),
            p.funcs.iter().map(|f| f.blocks.len()).sum::<usize>(),
        );
        assert_eq!(before, after);
    }
}
