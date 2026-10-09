//! Unit tests for `numrep`'s int32 slice (step 1) and guarded sums, checked against the
//! reference interpreter.

use super::*;
use crate::interp::{Host, Interp, Memory, Trap};
use crate::numrep::run_program;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::ExternFn;
use velt_vir::vir::{Program, Rvalue, Terminator};

/// Runs `velt_rt_math_to_int32` like the runtime (finite values below 2^63 are enough here).
struct ToInt32Host;

impl Host for ToInt32Host {
    fn call(&mut self, ext: &ExternFn, args: &[u64], _mem: &mut Memory) -> Result<u64, Trap> {
        assert_eq!(ext.symbol, TO_INT32);
        let x = f64::from_bits(args[0]);
        let v = if x.is_finite() { x as i64 as i32 } else { 0 };
        Ok(v as u32 as u64)
    }
}

/// `f(n, seed)`: `state = 0.0 + ((seed + 1) | 0)`, then `n` times `state = ((state ^ k) + acc) | 0`
/// and `acc = (acc + state) | 0` on `f64` locals; returns `acc + state` as `f64` bits.
fn hash_program() -> Program {
    let mut pb = ProgramBuilder::new();
    let to_int32 = pb.ext(TO_INT32, &[Ty::F64], Ty::I32, false);
    let mut fb = FuncBuilder::export("f", &[Ty::I64, Ty::I32], Ty::F64);
    let (n, seed) = (fb.param(0), fb.param(1));
    let (state, acc, i, c) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::I64),
        fb.local(Ty::Bool),
    );
    let (s32, w64, wf, sum, r32, rw) = (
        fb.local(Ty::I32),
        fb.local(Ty::I64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::I32),
        fb.local(Ty::F64),
    );
    let (out, a32) = (fb.local(Ty::F64), fb.local(Ty::I32));
    let (entry, head, body) = (fb.block(), fb.block(), fb.block());
    fb.assign(
        entry,
        s32,
        bin(BinOp::Add, copy_local(seed), int(1, Ty::I32)),
    );
    fb.assign(entry, w64, Rvalue::Cast(copy_local(s32), Ty::I64));
    fb.assign(entry, wf, Rvalue::Cast(copy_local(w64), Ty::F64));
    fb.assign(
        entry,
        state,
        bin(BinOp::Add, float(0.0, Ty::F64), copy_local(wf)),
    );
    fb.assign(entry, acc, Rvalue::Use(float(0.0, Ty::F64)));
    fb.assign(entry, i, Rvalue::Use(int(0, Ty::I64)));
    fb.goto(entry, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    let exit = fb.block();
    fb.branch(head, c, body, exit);
    // state = ToInt32(state + acc + 7)
    fb.assign(
        body,
        sum,
        bin(BinOp::Add, copy_local(state), copy_local(acc)),
    );
    fb.assign(
        body,
        sum,
        bin(BinOp::Add, copy_local(sum), float(7.0, Ty::F64)),
    );
    let b2 = fb.call(
        body,
        Callee::Extern(to_int32),
        vec![copy_local(sum)],
        Some(r32),
    );
    fb.assign(b2, rw, Rvalue::Cast(copy_local(r32), Ty::F64));
    fb.assign(b2, state, Rvalue::Use(copy_local(rw)));
    // acc = ToInt32(acc + state)
    fb.assign(b2, sum, bin(BinOp::Add, copy_local(acc), copy_local(state)));
    let b3 = fb.call(
        b2,
        Callee::Extern(to_int32),
        vec![copy_local(sum)],
        Some(a32),
    );
    fb.assign(b3, acc, Rvalue::Cast(copy_local(a32), Ty::F64));
    fb.assign(b3, i, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
    fb.goto(b3, head);
    fb.assign(
        exit,
        out,
        bin(BinOp::Add, copy_local(acc), copy_local(state)),
    );
    fb.ret(exit, copy_local(out));
    pb.add(fb.finish());
    pb.finish()
}

fn call(p: &Program, args: &[u64]) -> u64 {
    let mut interp = Interp::new(p, ToInt32Host);
    interp.call_symbol("f", args).expect("program runs")
}

fn optimized(p: &Program) -> Program {
    let mut q = p.clone();
    assert!(run_program(&mut q));
    assert_valid(&q);
    q
}

fn to_int32_calls(p: &Program) -> usize {
    p.funcs[0]
        .blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Call { .. }))
        .count()
}

#[test]
fn int32_locals_become_i32_with_the_same_results() {
    let p = hash_program();
    let q = optimized(&p);
    let f = &q.funcs[0];
    // `state` and `acc` are no longer assigned as f64 values.
    for l in [2usize, 3] {
        let assigned = f
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .any(|s| matches!(s, Stmt::Assign(d, _) if d.local.0 as usize == l));
        assert!(!assigned, "local {l} is still assigned");
    }
    for (n, seed) in [
        (0u64, 5u64),
        (1, 7),
        (50, 0x7fff_fffe),
        (1000, (-3i32) as u32 as u64),
    ] {
        assert_eq!(call(&p, &[n, seed]), call(&q, &[n, seed]), "f({n}, {seed})");
    }
}

#[test]
fn sums_of_int32_values_skip_the_conversion() {
    let p = hash_program();
    let q = optimized(&p);
    // `acc + state` adds two narrowed values: an integer add, no ToInt32 call. So does the
    // `state + acc + 7` chain, whose intermediate is a whole number below 2^34.
    assert_eq!(to_int32_calls(&p), 2);
    assert_eq!(to_int32_calls(&q), 0);
}

#[test]
fn locals_with_other_definitions_stay() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::F64], Ty::F64);
    let x = fb.param(0);
    let v = fb.local(Ty::F64);
    let b = fb.block();
    fb.assign(b, v, Rvalue::Use(float(1.0, Ty::F64)));
    fb.assign(b, v, bin(BinOp::Add, copy_local(v), copy_local(x)));
    fb.ret(b, copy_local(v));
    pb.add(fb.finish());
    let mut p = pb.finish();
    assert!(!run_program(&mut p));
}
