//! Random VIR through the optimizer (checked by the reference interpreter) and the backends.

use crate::common::builder::{int, FuncBuilder};
use crate::common::validate::validate;
use crate::random;
use velt_opt::interp::{Arg, Interp, RecordingHost, Trap};
use velt_opt::{optimize, OptLevel};
use velt_vir::vir::{Function, Program, Ty};

/// Steps the interpreter may take before a run counts as non-terminating.
const FUEL: u64 = 1_000_000;

/// A fuzzer input: the generator seed and the two `entry` arguments.
#[derive(Debug)]
pub struct Input {
    seed: u64,
    args: [u64; 2],
}

impl Input {
    /// Reads up to 24 little-endian bytes (missing bytes are zero).
    pub fn from_bytes(data: &[u8]) -> Self {
        let word = |i: usize| {
            let mut b = [0u8; 8];
            for (k, byte) in data.iter().skip(i * 8).take(8).enumerate() {
                b[k] = *byte;
            }
            u64::from_le_bytes(b)
        };
        Input {
            seed: word(0),
            args: [word(1), word(2)],
        }
    }
}

/// Every optimization level keeps the program valid and, when the original run finishes without
/// trapping, produces the same result and the same extern calls.
pub fn check_opt(input: &Input) {
    let original = random::program(input.seed);
    validate(&original)
        .unwrap_or_else(|e| panic!("generator produced invalid VIR: {e:?}\n{original}"));
    let Ok(before) = run(&original, &input.args) else {
        return;
    };
    for level in [OptLevel::None, OptLevel::Speed] {
        let mut opt = original.clone();
        optimize(&mut opt, level);
        validate(&opt).unwrap_or_else(|e| {
            panic!("{level:?} produced invalid VIR: {e:?}\n{original}\n--- optimized\n{opt}")
        });
        let after = run(&opt, &input.args);
        assert_eq!(
            Ok(&before),
            after.as_ref(),
            "{input:?} {level:?}\n{original}\n--- optimized\n{opt}"
        );
    }
}

/// Cranelift (plain and optimizing) and the LLVM IR emitter accept the program before and after
/// optimization.
pub fn check_codegen(input: &Input) {
    let mut original = random::program(input.seed);
    // The verifier every backend runs first requires the program entry point.
    original.funcs.push(main_stub());
    let mut opt = original.clone();
    optimize(&mut opt, OptLevel::Speed);
    let target = velt_codegen_cl::host_triple();
    for (p, what) in [(&original, "original"), (&opt, "optimized")] {
        for optimize in [false, true] {
            let opts = velt_codegen_cl::CodegenOptions {
                target: target.clone(),
                optimize,
            };
            if let Err(e) = velt_codegen_cl::emit_object(p, &opts) {
                panic!("cranelift (optimize={optimize}) rejected the {what} program: {e}\n{p}");
            }
        }
        if let Err(e) = velt_codegen_llvm::emit_ir(p, &target) {
            panic!("LLVM IR emission rejected the {what} program: {e}\n{p}");
        }
    }
}

/// `velt_main() -> i32 { return 0; }`.
fn main_stub() -> Function {
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let entry = fb.block();
    fb.ret(entry, int(0, Ty::I32));
    fb.finish()
}

/// Result and extern-call trace of `entry(args)`.
fn run(p: &Program, args: &[u64]) -> Result<(u64, Vec<String>), Trap> {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.set_fuel(FUEL);
    let result = interp.call_symbol("entry", args)?;
    let calls = interp
        .host
        .calls
        .iter()
        .map(|c| {
            let args: Vec<String> = c
                .args
                .iter()
                .map(|a| match a {
                    Arg::Ptr => "p".to_string(),
                    Arg::Bits(b) => b.to_string(),
                })
                .collect();
            format!("{}({})", c.symbol, args.join(","))
        })
        .collect();
    Ok((result, calls))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_seeds_pass() {
        for seed in 0..20 {
            let input = Input {
                seed,
                args: [seed, u64::MAX - seed],
            };
            check_opt(&input);
            check_codegen(&input);
        }
    }

    #[test]
    fn input_reads_little_endian_words() {
        let i = Input::from_bytes(&[1, 0, 0, 0, 0, 0, 0, 0, 2]);
        assert_eq!((i.seed, i.args), (1, [2, 0]));
    }
}
