//! Tests for `numrep`'s analysis and narrowing: hand-built functions whose shape is checked,
//! and a property test that runs random `f64` programs in the reference interpreter before and
//! after the pass (`random.rs`).

mod context;
mod random;

use super::*;
use crate::interp::{Host, Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, BlockId, Program, UnOp};

/// Runs the math externs like the runtime.
pub(super) struct MathHost;

impl Host for MathHost {
    fn call(&mut self, ext: &ExternFn, args: &[u64], _mem: &mut Memory) -> Result<u64, Trap> {
        let x = f64::from_bits(args[0]);
        Ok(match ext.symbol.as_str() {
            TO_INT32 => {
                let v = if x.is_finite() {
                    (x % 4294967296.0) as i64 as i32
                } else {
                    0
                };
                v as u32 as u64
            }
            "velt_rt_math_trunc" => x.trunc().to_bits(),
            "velt_rt_math_floor" => x.floor().to_bits(),
            "velt_rt_math_fabs" => x.abs().to_bits(),
            s => return Err(Trap::Invalid(format!("unexpected call to {s}"))),
        })
    }
}

/// `f(args)` in the interpreter, as raw bits.
pub(super) fn call(p: &Program, args: &[u64]) -> Result<u64, Trap> {
    let mut i = Interp::new(p, MathHost);
    i.set_fuel(100_000);
    i.call_symbol("f", args)
}

fn narrowed(p: &Program) -> Program {
    let mut q = p.clone();
    run_program(&mut q);
    assert_valid(&q);
    q
}

fn local_ty_count(f: &Function, ty: Ty) -> usize {
    f.locals.iter().filter(|l| l.ty == ty).count()
}

/// Whether some statement still assigns local `l`.
fn assigned(f: &Function, l: Local) -> bool {
    f.blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == l))
}

/// `f(n)`: `s = 0.0; for (k = 0.0; k < 1000.0; k += 1.0) s += k; return s + n`.
fn counter_program(start: f64, bound: f64) -> (Program, Local, Local) {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::F64], Ty::F64);
    let n = fb.param(0);
    let (s, k, c, t, u, r) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
    );
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.assign(entry, k, Rvalue::Use(float(start, Ty::F64)));
    fb.goto(entry, head);
    fb.assign(
        head,
        c,
        bin(BinOp::Lt, copy_local(k), float(bound, Ty::F64)),
    );
    fb.branch(head, c, body, exit);
    fb.assign(body, t, bin(BinOp::Add, copy_local(s), copy_local(k)));
    fb.assign(body, s, Rvalue::Use(copy_local(t)));
    fb.assign(body, u, bin(BinOp::Add, copy_local(k), float(1.0, Ty::F64)));
    fb.assign(body, k, Rvalue::Use(copy_local(u)));
    fb.goto(body, head);
    fb.assign(exit, r, bin(BinOp::Add, copy_local(s), copy_local(n)));
    fb.ret(exit, copy_local(r));
    pb.add(fb.finish());
    (pb.finish(), s, k)
}

#[test]
fn bounded_counters_become_i32_and_sums_stay_doubles() {
    let (p, s, k) = counter_program(0.0, 1000.0);
    let q = narrowed(&p);
    let f = &q.funcs[0];
    assert!(!assigned(f, k), "the counter is narrowed");
    assert!(assigned(f, s), "the unbounded sum stays a double");
    assert!(
        local_ty_count(f, Ty::I32) >= 2,
        "counter and its increment are i32"
    );
    for n in [0.0, -0.0, 2.5, f64::NAN] {
        let a = [f64::to_bits(n)];
        assert_eq!(call(&p, &a), call(&q, &a), "f({n})");
    }
}

#[test]
fn counters_reaching_2_53_stay_doubles() {
    // k + 1 stops growing at 2^53 in doubles; an integer would not.
    let two_53 = 9_007_199_254_740_992.0;
    let (p, _, k) = counter_program(two_53 - 3.0, two_53 + 2.0);
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], k));
}

#[test]
fn fractional_steps_stay_doubles() {
    let (p, _, k) = counter_program(0.5, 10.0);
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], k));
}

/// `f(a: i64)`: `x = (a as f64 clamped to [-10, 10]) % 3`, then `g(x)` where `use_` builds the
/// use of `x` and returns the result.
fn remainder_program(use_: impl Fn(&mut FuncBuilder, BlockId, Local) -> Local) -> (Program, Local) {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::I64], Ty::F64);
    let a = fb.param(0);
    let (lo_ok, hi_ok, af, x) = (
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
    );
    let (entry, mid, body, out) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(
        entry,
        lo_ok,
        bin(BinOp::Ge, copy_local(a), int(-10, Ty::I64)),
    );
    fb.branch(entry, lo_ok, mid, out);
    fb.assign(mid, hi_ok, bin(BinOp::Le, copy_local(a), int(10, Ty::I64)));
    fb.branch(mid, hi_ok, body, out);
    fb.assign(body, af, Rvalue::Cast(copy_local(a), Ty::F64));
    fb.assign(
        body,
        x,
        bin(BinOp::Rem, copy_local(af), float(3.0, Ty::F64)),
    );
    let r = use_(&mut fb, body, x);
    fb.ret(body, copy_local(r));
    fb.ret(out, float(0.5, Ty::F64));
    pb.add(fb.finish());
    (pb.finish(), x)
}

fn all_remainders_agree(p: &Program, q: &Program) {
    for a in -12i64..=12 {
        let args = [a as u64];
        assert_eq!(call(p, &args), call(q, &args), "f({a})");
    }
}

#[test]
fn negative_zero_returned_keeps_the_double() {
    // -3 % 3 is -0, and returning it shows it.
    let (p, x) = remainder_program(|_, _, x| x);
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], x));
    all_remainders_agree(&p, &q);
}

#[test]
fn negative_zero_that_no_read_sees_is_narrowed() {
    // (x % 3) + 1 cannot be -0 and does not show x's sign: x becomes an integer.
    let (p, x) = remainder_program(|fb, b, x| {
        let y = fb.local(Ty::F64);
        fb.assign(b, y, bin(BinOp::Add, copy_local(x), float(1.0, Ty::F64)));
        y
    });
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], x));
    all_remainders_agree(&p, &q);
}

#[test]
fn negative_zero_seen_by_a_division_keeps_the_double() {
    // 1 / (x % 3) is -Infinity for -0.
    let (p, x) = remainder_program(|fb, b, x| {
        let y = fb.local(Ty::F64);
        fb.assign(b, y, bin(BinOp::Div, float(1.0, Ty::F64), copy_local(x)));
        y
    });
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], x));
    all_remainders_agree(&p, &q);
}

#[test]
fn remainders_that_may_be_nan_stay_doubles() {
    // a % (a + 0.0) is NaN for a = 0.
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::I32], Ty::Bool);
    let a = fb.param(0);
    let (af, x, c) = (fb.local(Ty::F64), fb.local(Ty::F64), fb.local(Ty::Bool));
    let b = fb.block();
    fb.assign(b, af, Rvalue::Cast(copy_local(a), Ty::F64));
    fb.assign(b, x, bin(BinOp::Rem, copy_local(af), copy_local(af)));
    fb.assign(b, c, bin(BinOp::Eq, copy_local(x), copy_local(x)));
    fb.ret(b, copy_local(c));
    pb.add(fb.finish());
    let p = pb.finish();
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], x));
    for v in [0i32, 1, -5] {
        assert_eq!(call(&p, &[v as u32 as u64]), call(&q, &[v as u32 as u64]));
    }
}

/// `f(a: i32)`: `x = trunc(a as f64)`, then `x < 2^40 ? x : -1` through a branch.
fn rounding_program() -> Program {
    let mut pb = ProgramBuilder::new();
    let trunc = pb.ext("velt_rt_math_trunc", &[Ty::F64], Ty::F64, false);
    let mut fb = FuncBuilder::export("f", &[Ty::I32], Ty::F64);
    let a = fb.param(0);
    let (af, x, c) = (fb.local(Ty::F64), fb.local(Ty::F64), fb.local(Ty::Bool));
    let b0 = fb.block();
    fb.assign(b0, af, Rvalue::Cast(copy_local(a), Ty::F64));
    let b1 = fb.call(b0, Callee::Extern(trunc), vec![copy_local(af)], Some(x));
    fb.assign(
        b1,
        c,
        bin(BinOp::Lt, copy_local(x), float(1099511627776.0, Ty::F64)),
    );
    let (yes, no) = (fb.block(), fb.block());
    fb.branch(b1, c, yes, no);
    fb.ret(yes, copy_local(x));
    fb.ret(no, float(-1.0, Ty::F64));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn facts_fold_rounding_calls_and_decided_branches() {
    let p = rounding_program();
    let q = narrowed(&p);
    let f = &q.funcs[0];
    assert_eq!(count_calls(f), 0, "trunc of a whole number is the number");
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

#[test]
fn report_lists_unnarrowed_named_locals_in_loops() {
    let (mut p, s, _) = counter_program(0.0, 1000.0);
    p.funcs[0].locals[s.0 as usize].name = Some("s".into());
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let r = unnarrowed(&env, &p.funcs[0]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].name, "s");
    assert!(r[0].reason.contains("2^53"), "{}", r[0].reason);
}

/// `f(a: i64)`: 1.0 when `a < below && a <= at_most`, 2.0 when only `a < below`, else `a` as a
/// double (which makes `a` a tracked value).
fn strict_below_program(below: i64, at_most: i64) -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::I64], Ty::F64);
    let a = fb.param(0);
    let (af, c, d) = (fb.local(Ty::F64), fb.local(Ty::Bool), fb.local(Ty::Bool));
    let (entry, inner, one, two, out) =
        (fb.block(), fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, af, Rvalue::Cast(copy_local(a), Ty::F64));
    fb.assign(
        entry,
        c,
        bin(BinOp::Lt, copy_local(a), int(below as i128, Ty::I64)),
    );
    fb.branch(entry, c, inner, out);
    fb.assign(
        inner,
        d,
        bin(BinOp::Le, copy_local(a), int(at_most as i128, Ty::I64)),
    );
    fb.branch(inner, d, one, two);
    fb.ret(one, float(1.0, Ty::F64));
    fb.ret(two, float(2.0, Ty::F64));
    fb.ret(out, copy_local(af));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn strict_integer_comparisons_past_2_53_keep_the_integers_in_between() {
    // `a < 2^60` allows `a = 2^60 - 1`, above the double just below 2^60 (2^60 - 128); and
    // `a < -2^53` allows `-2^53 - 1`, above the double just below -2^53 (-2^53 - 2).
    let two_60 = 1i64 << 60;
    let two_53 = 1i64 << 53;
    for (below, at_most, a) in [
        (two_60, two_60 - 128, two_60 - 1),
        (-two_53, -two_53 - 2, -two_53 - 1),
    ] {
        let p = strict_below_program(below, at_most);
        let q = narrowed(&p);
        let args = [a as u64];
        assert_eq!(call(&p, &args), Ok(2.0f64.to_bits()), "f({a}) before");
        assert_eq!(call(&q, &args), Ok(2.0f64.to_bits()), "f({a}) after");
    }
}

#[test]
fn a_conversion_bounded_by_2_53_does_not_bound_the_integer() {
    // `a >= 0 && (a as f64) <= 2^53 && a <= 2^53`: `2^53 + 1` converts to 2^53, so the last
    // test is not implied.
    let two_53 = 1i128 << 53;
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::I64], Ty::F64);
    let a = fb.param(0);
    let (c0, x, c1, d) = (
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
    );
    let (entry, mid, inner, one, two, out) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(entry, c0, bin(BinOp::Ge, copy_local(a), int(0, Ty::I64)));
    fb.branch(entry, c0, mid, out);
    fb.assign(mid, x, Rvalue::Cast(copy_local(a), Ty::F64));
    fb.assign(
        mid,
        c1,
        bin(BinOp::Le, copy_local(x), float(two_53 as f64, Ty::F64)),
    );
    fb.branch(mid, c1, inner, out);
    fb.assign(
        inner,
        d,
        bin(BinOp::Le, copy_local(a), int(two_53, Ty::I64)),
    );
    fb.branch(inner, d, one, two);
    fb.ret(one, float(1.0, Ty::F64));
    fb.ret(two, float(2.0, Ty::F64));
    fb.ret(out, float(0.0, Ty::F64));
    pb.add(fb.finish());
    let p = pb.finish();
    let q = narrowed(&p);
    for a in [two_53 as i64 - 1, two_53 as i64, two_53 as i64 + 1] {
        let args = [a as u64];
        assert_eq!(call(&p, &args), call(&q, &args), "f({a})");
    }
    assert_eq!(
        call(&q, &[(two_53 as i64 + 1) as u64]),
        Ok(2.0f64.to_bits())
    );
}

/// `f(a)`: `((a >> 2) as f64) % 7.0` for `a: u64` (whole, up to 2^62: past 2^53), or with
/// `a: i64` (`a as f64`, which may be negative).
fn remainder_of_whole_program(signed: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let ty = if signed { Ty::I64 } else { Ty::U64 };
    let mut fb = FuncBuilder::export("f", &[ty], Ty::F64);
    let a = fb.param(0);
    let (s, af, x) = (fb.local(ty), fb.local(Ty::F64), fb.local(Ty::F64));
    let b = fb.block();
    if signed {
        fb.assign(b, s, Rvalue::Use(copy_local(a)));
    } else {
        fb.assign(b, s, bin(BinOp::UShr, copy_local(a), int(2, Ty::U64)));
    }
    fb.assign(b, af, Rvalue::Cast(copy_local(s), Ty::F64));
    fb.assign(b, x, bin(BinOp::Rem, copy_local(af), float(7.0, Ty::F64)));
    fb.ret(b, copy_local(x));
    pb.add(fb.finish());
    pb.finish()
}

fn float_remainders(f: &Function) -> usize {
    f.blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| {
            matches!(s, Stmt::Assign(d, Rvalue::Binary(BinOp::Rem, ..))
                if f.locals[d.local.0 as usize].ty == Ty::F64)
        })
        .count()
}

#[test]
fn remainders_of_whole_non_negative_doubles_use_integers() {
    let p = remainder_of_whole_program(false);
    let q = narrowed(&p);
    assert_eq!(float_remainders(&q.funcs[0]), 0);
    for a in [
        0u64,
        1,
        6,
        7,
        29,
        1 << 55,
        u64::MAX,
        u64::MAX - 5,
        (1 << 54) + 3,
    ] {
        assert_eq!(call(&p, &[a]), call(&q, &[a]), "f({a})");
    }
}

#[test]
fn remainders_of_doubles_that_may_be_negative_stay_doubles() {
    // -7 % 7 is -0, which the integer remainder is not.
    let p = remainder_of_whole_program(true);
    let q = narrowed(&p);
    assert_eq!(float_remainders(&q.funcs[0]), 1);
    for a in [-7i64, -8, 0, 7, i64::MIN] {
        assert_eq!(call(&p, &[a as u64]), call(&q, &[a as u64]), "f({a})");
    }
}

/// `f(a: i32, out: ptr)`: `x.toString()` and `` `${x}` `` of `x = a as f64` (or of `x * 0.5`).
fn formatted_program(half: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let from = pb.ext("velt_rt_str_from_f64", &[Ty::F64, Ty::Ptr], Ty::Unit, false);
    let push = pb.ext(
        "velt_rt_strbuf_push_f64",
        &[Ty::Ptr, Ty::F64],
        Ty::Unit,
        false,
    );
    let mut fb = FuncBuilder::export("f", &[Ty::I32, Ty::Ptr], Ty::Unit);
    let (a, out) = (fb.param(0), fb.param(1));
    let (af, x) = (fb.local(Ty::F64), fb.local(Ty::F64));
    let b0 = fb.block();
    fb.assign(b0, af, Rvalue::Cast(copy_local(a), Ty::F64));
    let factor = if half { 0.5 } else { 1.0 };
    fb.assign(
        b0,
        x,
        bin(BinOp::Mul, copy_local(af), float(factor, Ty::F64)),
    );
    let args = vec![copy_local(x), copy_local(out)];
    let b1 = fb.call(b0, Callee::Extern(from), args, None);
    let args = vec![copy_local(out), copy_local(x)];
    let b2 = fb.call(b1, Callee::Extern(push), args, None);
    fb.ret(b2, Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit));
    pb.add(fb.finish());
    let mut p = pb.finish();
    print::declare(&mut p.externs);
    p
}

fn callees(p: &Program) -> Vec<String> {
    p.funcs[0]
        .blocks
        .iter()
        .filter_map(|b| match &b.term {
            Terminator::Call {
                callee: Callee::Extern(id),
                ..
            } => Some(p.externs[id.0 as usize].symbol.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn whole_numbers_format_through_the_integer_formatters() {
    let q = narrowed(&formatted_program(false));
    assert_eq!(
        callees(&q),
        ["velt_rt_str_from_i64", "velt_rt_strbuf_push_i64"]
    );
    let q = narrowed(&formatted_program(true));
    assert_eq!(
        callees(&q),
        ["velt_rt_str_from_f64", "velt_rt_strbuf_push_f64"]
    );
}
