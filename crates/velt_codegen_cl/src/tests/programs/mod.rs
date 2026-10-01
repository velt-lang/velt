//! Hand-written VIR test programs with their expected stdout, shared by the JIT, object
//! and native-link tests.

mod arith;
mod control;
mod dynmem;
mod memory;

pub(crate) use arith::{casts, float_ops, int_ops, math};
pub(crate) use control::{fib, indirect, switch};
pub(crate) use dynmem::{mem_ops, vtables};
pub(crate) use memory::{aggregates, strings};

use super::*;
use velt_vir::vir::Ty::*;

/// A program plus its expected behaviour.
pub(crate) struct TestProgram {
    /// Name used in assertion messages.
    pub name: &'static str,
    /// The VIR program (entry `velt_main`).
    pub program: Program,
    /// Expected stdout.
    pub stdout: String,
    /// Expected exit code (return value of `velt_main`).
    pub exit: i32,
}

/// Every test program.
pub(crate) fn all() -> Vec<TestProgram> {
    vec![
        fib(),
        int_ops(),
        float_ops(),
        math(),
        casts(),
        aggregates(),
        switch(),
        indirect(),
        strings(),
        vtables(),
        mem_ops(),
    ]
}

fn bin(op: BinOp, a: Operand, b: Operand) -> Rvalue {
    Rvalue::Binary(op, a, b)
}

// ───────────── integer ops ─────────────

/// `fn(a: ta, b: tb) -> rt { return a op b }`
fn binary_fn(pb: &mut ProgramBuilder, op: BinOp, ta: Ty, tb: Ty) -> (FuncId, Ty) {
    let rt = if matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    ) {
        Bool
    } else {
        ta
    };
    let n = pb.p.funcs.len();
    let mut fb = FuncBuilder::internal(&format!("bin{n}_{op:?}_{ta:?}"), &[ta, tb], rt);
    let r = fb.local(rt);
    let b = fb.block();
    fb.assign(b, r, bin(op, copy_local(Local(0)), copy_local(Local(1))));
    fb.term(b, Terminator::Return(copy_local(r)));
    (pb.add(fb.finish()), rt)
}

fn unary_fn(pb: &mut ProgramBuilder, op: UnOp, ty: Ty) -> FuncId {
    let n = pb.p.funcs.len();
    let mut fb = FuncBuilder::internal(&format!("un{n}"), &[ty], ty);
    let r = fb.local(ty);
    let b = fb.block();
    fb.assign(b, r, Rvalue::Unary(op, copy_local(Local(0))));
    fb.term(b, Terminator::Return(copy_local(r)));
    pb.add(fb.finish())
}

fn cast_fn(pb: &mut ProgramBuilder, from: Ty, to: Ty) -> FuncId {
    let n = pb.p.funcs.len();
    let mut fb = FuncBuilder::internal(&format!("cast{n}"), &[from], to);
    let r = fb.local(to);
    let b = fb.block();
    fb.assign(b, r, Rvalue::Cast(copy_local(Local(0)), to));
    fb.term(b, Terminator::Return(copy_local(r)));
    pb.add(fb.finish())
}

/// Accumulates "call f(args); print result" steps in `velt_main`.
struct Cases<'a> {
    o: Out<'a>,
    expected: String,
}

impl Cases<'_> {
    fn call(&mut self, f: FuncId, args: Vec<Operand>, rty: Ty, expect: &str) {
        let r = self.o.fb.local(rty);
        self.o.cur = self
            .o
            .fb
            .call(self.o.cur, Callee::Func(f), args, Some(Place::local(r)));
        self.o.line(copy_local(r), rty);
        self.expected.push_str(expect);
        self.expected.push('\n');
    }
}

fn operand(v: f64, ty: Ty) -> Operand {
    if ty.is_float() {
        float(v, ty)
    } else {
        int(v as i128, ty)
    }
}
