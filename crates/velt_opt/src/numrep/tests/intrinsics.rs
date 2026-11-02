//! `numrep` on math operations as statements (`Rvalue::Intrinsic`, #1981): every program of
//! these tests, with its math calls rewritten into that form, narrows and folds as the call
//! form does and computes the same results.

use super::*;
use velt_vir::vir::{Intrinsic, Terminator};

#[test]
fn rounding_of_a_whole_value_folds_as_a_statement() {
    let p = rounding_program();
    assert_eq!(math_ops(&p.funcs[0]), 1);
    let q = narrowed(&p);
    let f = &q.funcs[0];
    assert_eq!(math_ops(f), 0, "trunc of a whole number is the number");
    assert!(
        !f.blocks
            .iter()
            .any(|b| matches!(b.term, Terminator::Branch { .. })),
        "an int32 is always below 2^40"
    );
    for v in [0i32, -7, i32::MAX, i32::MIN] {
        assert_eq!(call(&p, &[v as u32 as u64]), call(&q, &[v as u32 as u64]));
    }
}

/// `f(a: i64, b: i64) -> i32`: `ToInt32((a as f64) + (b as f64))`, the sum `int32::guarded_sums`
/// splits, with code after it in the block.
fn guarded_sum_program() -> Program {
    let mut pb = ProgramBuilder::new();
    pb.ext(TO_INT32, &[Ty::F64], Ty::I32, false);
    let mut fb = FuncBuilder::export("f", &[Ty::I64, Ty::I64], Ty::I32);
    let (a, b) = (fb.param(0), fb.param(1));
    let (x, y, t, r, s) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::I32),
        fb.local(Ty::I32),
    );
    let b0 = fb.block();
    fb.assign(b0, x, Rvalue::Cast(copy_local(a), Ty::F64));
    fb.assign(b0, y, Rvalue::Cast(copy_local(b), Ty::F64));
    fb.assign(b0, t, bin(BinOp::Add, copy_local(x), copy_local(y)));
    let op = Intrinsic::Math(velt_vir::math::MathOp::ToInt32);
    fb.assign(b0, r, Rvalue::Intrinsic(op, vec![copy_local(t)]));
    fb.assign(b0, s, bin(BinOp::Add, copy_local(r), int(1, Ty::I32)));
    fb.ret(b0, copy_local(s));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn int32_sums_are_guarded_as_statements() {
    let p = guarded_sum_program();
    let mut q = p.clone();
    assert!(int32::guarded_sums(&mut q.funcs[0]));
    assert_valid(&q);
    let f = &q.funcs[0];
    assert!(
        (f.blocks.iter()).any(|b| matches!(b.term, Terminator::Branch { .. })),
        "the sum is guarded\n{q}"
    );
    let big = 1i64 << 53;
    for (x, y) in [
        (0i64, 0i64),
        (5, -7),
        (i64::from(i32::MAX), 1),
        (big, 1),
        (-big, 3),
        (big + 2, big),
    ] {
        let args = [x as u64, y as u64];
        assert_eq!(call(&p, &args), call(&q, &args), "f({x}, {y})");
    }
}

#[test]
fn int32_programs_narrow_as_statements() {
    // `hash_program` of the int32 tests, its ToInt32 calls as statements: the two loop
    // variables still become integers and the results stay the same.
    let p = crate::numrep::int32::tests::hash_program();
    let q = narrowed(&p);
    assert_eq!(math_ops(&q.funcs[0]), 0, "{q}");
    for (n, seed) in [
        (0u64, 5u64),
        (1, 7),
        (50, 0x7fff_fffe),
        (1000, (-3i32) as u32 as u64),
    ] {
        assert_eq!(call(&p, &[n, seed]), call(&q, &[n, seed]), "f({n}, {seed})");
    }
}
