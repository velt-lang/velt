//! Hand-written VIR programs covering loops, switches, recursion, aggregates through
//! pointers, statics, direct/indirect/extern calls, closures and constant-heavy arithmetic.
//! Each is run before and after optimization by the semantics tests.
// Shared by several test binaries, each using a subset.
#![allow(dead_code)]

mod closures;
pub mod control;
mod dynmem;
mod memory;
mod scalar;

use crate::common::builder::*;
use velt_vir::vir::*;

/// A program and the calls (entry symbol, raw-bit arguments) to run it with.
pub struct Case {
    /// Name for assertion messages.
    pub name: &'static str,
    /// The program.
    pub program: Program,
    /// Runs: (exported function symbol, arguments).
    pub runs: Vec<(&'static str, Vec<u64>)>,
}

impl Case {
    /// Add runs of `entry` with each argument list.
    pub fn with(mut self, entry: &'static str, inputs: Vec<Vec<u64>>) -> Case {
        self.runs
            .extend(inputs.into_iter().map(|args| (entry, args)));
        self
    }
}

/// Every corpus program.
pub fn all() -> Vec<Case> {
    let mut cases = scalar::cases();
    cases.extend(control::cases());
    cases.extend(memory::cases());
    cases.extend(dynmem::cases());
    cases.extend(closures::cases());
    cases
}

/// A program under construction with a few runtime-like externs declared.
pub struct Env {
    /// The builder.
    pub pb: ProgramBuilder,
    /// `write_i64(I64)`.
    pub write_i64: ExternId,
    /// `write_f64(F64)`.
    pub write_f64: ExternId,
    /// `panic(Ptr) -> noreturn`.
    pub panic: ExternId,
}

impl Env {
    /// Fresh program with the externs.
    pub fn new() -> Env {
        let mut pb = ProgramBuilder::new();
        let write_i64 = pb.ext("write_i64", &[Ty::I64], Ty::Unit, false);
        let write_f64 = pb.ext("write_f64", &[Ty::F64], Ty::Unit, false);
        let panic = pb.ext("panic", &[Ty::Ptr], Ty::Unit, true);
        Env {
            pb,
            write_i64,
            write_f64,
            panic,
        }
    }

    /// Emit `write_i64(op as i64)` at the end of `b`; returns the continuation.
    pub fn print(&self, fb: &mut FuncBuilder, b: BlockId, op: Operand, ty: Ty) -> BlockId {
        let arg = if ty == Ty::I64 {
            op
        } else {
            let w = fb.local(Ty::I64);
            fb.assign(b, w, Rvalue::Cast(op, Ty::I64));
            copy_local(w)
        };
        fb.call(b, Callee::Extern(self.write_i64), vec![arg], None)
    }

    /// Emit `t = rv` then print `t`; returns the continuation.
    pub fn show(&self, fb: &mut FuncBuilder, b: BlockId, rv: Rvalue, ty: Ty) -> BlockId {
        let t = fb.local(ty);
        fb.assign(b, t, rv);
        self.print(fb, b, copy_local(t), ty)
    }

    /// Finish into a case.
    pub fn case(self, name: &'static str, entry: &'static str, inputs: Vec<Vec<u64>>) -> Case {
        Case {
            name,
            program: self.pb.finish(),
            runs: vec![],
        }
        .with(entry, inputs)
    }
}

/// Exported `velt_main() -> I32` builder with its entry block.
pub fn main_fn() -> (FuncBuilder, BlockId) {
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let b = fb.block();
    (fb, b)
}

/// Raw bits of an `I64` argument.
pub fn arg(v: i64) -> u64 {
    v as u64
}

/// `Const::Unit` operand.
pub fn unit() -> Operand {
    Operand::Const(Const::Unit, Ty::Unit)
}
