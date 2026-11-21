//! The start-up CPU check (rt_abi.md "CPU check"): std's start-up hook (std/lang/cpu, which
//! `velt_main` calls first thing) compares the CPU's features with what the program was built for,
//! `velt_simd_cpu_required()`, so on a CPU without them it exits with a message naming what is
//! missing instead of dying of an illegal instruction. That value is this build's: here, right
//! before code generation, every call of it becomes the constant `--cpu` gives (x86's mask over
//! `velt_codegen_cl::X86_FEATURES` in the low half, arm64's over `ARM_FEATURES` in the high half,
//! the feature word's layout). The `velt dev` JIT, which runs on the machine it compiles for,
//! leaves it to the backend, where it is 0.

use velt_vir::simd::SimdCall;
use velt_vir::vir::{Callee, Const, Operand, Program, Rvalue, Stmt, Terminator, Ty};

/// The feature word bits of `required` (a mask over the start-up check's list of `target`'s
/// architecture): arm64's sit above x86's.
fn word_bits(required: u32, target: &str) -> u64 {
    let arm = matches!(target.split('-').next(), Some("aarch64" | "arm64"));
    u64::from(required) << if arm { 32 } else { 0 }
}

/// Replace every call of `velt_simd_cpu_required()` in `program` with the feature word bits of
/// `required`, this build's CPU requirement for `target` (0 for the baseline).
pub(super) fn fill(program: &mut Program, required: u32, target: &str) {
    let value = word_bits(required, target);
    let is_required = |program: &Program, callee: &Callee| match callee {
        Callee::Extern(id) => matches!(
            SimdCall::of_symbol(&program.ext(*id).symbol),
            Some(SimdCall::CpuRequired)
        ),
        _ => false,
    };
    for fi in 0..program.funcs.len() {
        for bi in 0..program.funcs[fi].blocks.len() {
            let Terminator::Call {
                callee, dest, next, ..
            } = &program.funcs[fi].blocks[bi].term
            else {
                continue;
            };
            if !is_required(program, callee) {
                continue;
            }
            let (dest, next) = (dest.clone(), *next);
            let func = &mut program.funcs[fi];
            if let Some(dest) = dest {
                let c = Operand::Const(Const::Int(value.into()), Ty::U64);
                func.blocks[bi]
                    .stmts
                    .push(Stmt::Assign(dest, Rvalue::Use(c)));
                // The call's location goes to the statement, and stays the `goto`'s.
                if let Some(locs) = func.locs.get_mut(bi) {
                    let at = locs.last().copied().flatten();
                    locs.insert(locs.len().saturating_sub(1), at);
                }
            }
            func.blocks[bi].term = Terminator::Goto(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{compile, BuildOptions, Session};

    fn program() -> Program {
        let opts = BuildOptions {
            input: "main.velt".into(),
            root_source: Some("function main() {\n  console.log(\"hi\");\n}\n".into()),
            ..Default::default()
        };
        let mut sess = Session::new();
        compile(&mut sess, &opts).unwrap_or_else(|_| panic!("{}", sess.render_diagnostics()))
    }

    /// The calls of `velt_simd_cpu_required` in `p`.
    fn calls(p: &Program) -> usize {
        (p.funcs.iter().flat_map(|f| &f.blocks))
            .filter(|b| match &b.term {
                Terminator::Call {
                    callee: Callee::Extern(id),
                    ..
                } => p.ext(*id).symbol == "velt_simd_cpu_required",
                _ => false,
            })
            .count()
    }

    #[test]
    fn every_program_asks_and_the_driver_answers() {
        let mut p = program();
        assert!(calls(&p) > 0, "std's start-up hook reads the requirement");
        fill(&mut p, 0x3f, "x86_64-unknown-linux-gnu");
        velt_vir::verify(&p).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(calls(&p), 0);
        let text = p.to_string();
        assert!(text.contains(" = 63_u64\n    goto"), "{text}");
    }

    #[test]
    fn arm64_bits_sit_above_x86_ones() {
        assert_eq!(word_bits(1, "aarch64-apple-darwin"), 1 << 32);
        assert_eq!(word_bits(0x3f, "x86_64-pc-windows-msvc"), 0x3f);
        assert_eq!(word_bits(0, "aarch64-unknown-linux-gnu"), 0);
    }
}
