//! Facts `numrep` takes from the context of a value (#525 step 3): constants every call passes
//! a parameter, the length of an array, a copy of a compared value, ToInt32 of a constant, and
//! the width of a counter added to a 64-bit sum.

use super::*;
use velt_vir::vir::{Const, Proj, StaticData};

/// A loop `for (k = 0; k < bound; k++) s += k` over doubles, `bound` an operand, in new blocks
/// (the first one is its entry); returns `k`.
fn counting(fb: &mut FuncBuilder, bound: Operand) -> Local {
    let (s, k, c, t, u) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
    );
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.assign(entry, k, Rvalue::Use(float(0.0, Ty::F64)));
    fb.goto(entry, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(k), bound));
    fb.branch(head, c, body, exit);
    fb.assign(body, t, bin(BinOp::Add, copy_local(s), copy_local(k)));
    fb.assign(body, s, Rvalue::Use(copy_local(t)));
    fb.assign(body, u, bin(BinOp::Add, copy_local(k), float(1.0, Ty::F64)));
    fb.assign(body, k, Rvalue::Use(copy_local(u)));
    fb.goto(body, head);
    fb.ret(exit, copy_local(s));
    k
}

/// `g(n)` counts to its parameter; `f()` calls `g(1000)` (and, with `other`, also `g(x)`).
fn param_program(other: bool) -> (Program, Local) {
    let mut pb = ProgramBuilder::new();
    let g_id = pb.reserve();
    let mut gb = FuncBuilder::internal("g", &[Ty::F64], Ty::F64);
    let n = gb.param(0);
    let k = counting(&mut gb, copy_local(n));
    pb.set(g_id, gb.finish());
    let mut fb = FuncBuilder::export("f", &[Ty::F64], Ty::F64);
    let x = fb.param(0);
    let (a, b) = (fb.local(Ty::F64), fb.local(Ty::F64));
    let b0 = fb.block();
    let b1 = fb.call(
        b0,
        Callee::Func(g_id),
        vec![float(1000.0, Ty::F64)],
        Some(a),
    );
    let arg = if other {
        copy_local(x)
    } else {
        float(10.0, Ty::F64)
    };
    let b2 = fb.call(b1, Callee::Func(g_id), vec![arg], Some(b));
    fb.ret(b2, copy_local(a));
    pb.add(fb.finish());
    (pb.finish(), k)
}

#[test]
fn constant_arguments_bound_a_parameter() {
    let (p, k) = param_program(false);
    let q = narrowed(&p);
    assert!(
        !assigned(&q.funcs[0], k),
        "k < n with n in {{10, 1000}} narrows k"
    );
    let (p, k) = param_program(true);
    let q = narrowed(&p);
    assert!(
        assigned(&q.funcs[0], k),
        "a call with an unknown argument keeps k a double"
    );
}

#[test]
fn an_address_taken_function_has_no_parameter_facts() {
    let (mut p, k) = param_program(false);
    let g = velt_vir::vir::FuncId(0);
    p.statics.push(StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: vec![(0, Const::Func(g))],
    });
    let q = narrowed(&p);
    assert!(
        assigned(&q.funcs[0], k),
        "a vtable may call it with anything"
    );
}

/// `f(xs)`: `for (k = 0; k < xs.length; k++) s += k`, `xs` an array (field 1 its length).
fn length_program(array_agg: &str) -> (Program, Local) {
    let mut pb = ProgramBuilder::new();
    let arr = pb.agg(
        array_agg,
        24,
        8,
        &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)],
    );
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::F64);
    let xs = fb.param(0);
    let (len, lenf) = (fb.local(Ty::U64), fb.local(Ty::F64));
    let pre = fb.block();
    let mut p = deref(xs, Ty::Agg(arr));
    p.proj.push(Proj::Field(1));
    fb.assign(pre, len, Rvalue::Use(copy_place(p)));
    fb.assign(pre, lenf, Rvalue::Cast(copy_local(len), Ty::F64));
    let k = counting(&mut fb, copy_local(lenf));
    fb.goto(pre, BlockId(1));
    pb.add(fb.finish());
    (pb.finish(), k)
}

#[test]
fn an_array_length_bounds_its_index_loop() {
    let (p, k) = length_program(velt_vir::ARRAY_AGG_NAME);
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], k), "k < xs.length narrows k");
    let (p, k) = length_program("not an array");
    let q = narrowed(&p);
    assert!(
        assigned(&q.funcs[0], k),
        "field 1 of another aggregate bounds nothing"
    );
}

#[test]
fn a_copy_of_a_compared_value_is_refined_too() {
    // s = 0; loop { t = s + 1; s = t; if (t >= 1000) { u = s - 1000; s = u } } with a counter
    // bounding the loop: `s` is refined through its copy of `t`.
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[], Ty::F64);
    let (s, t, u, c, d) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
    );
    let i = fb.local(Ty::F64);
    let (entry, head, body, wrap, next, exit) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(entry, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.assign(entry, i, Rvalue::Use(float(0.0, Ty::F64)));
    fb.goto(entry, head);
    fb.assign(
        head,
        d,
        bin(BinOp::Lt, copy_local(i), float(100.0, Ty::F64)),
    );
    fb.branch(head, d, body, exit);
    fb.assign(body, t, bin(BinOp::Add, copy_local(s), float(7.0, Ty::F64)));
    fb.assign(body, s, Rvalue::Use(copy_local(t)));
    fb.assign(
        body,
        c,
        bin(BinOp::Ge, copy_local(t), float(1000.0, Ty::F64)),
    );
    fb.branch(body, c, wrap, next);
    fb.assign(
        wrap,
        u,
        bin(BinOp::Sub, copy_local(s), float(1000.0, Ty::F64)),
    );
    fb.assign(wrap, s, Rvalue::Use(copy_local(u)));
    fb.goto(wrap, next);
    let j = fb.local(Ty::F64);
    fb.assign(next, j, bin(BinOp::Add, copy_local(i), float(1.0, Ty::F64)));
    fb.assign(next, i, Rvalue::Use(copy_local(j)));
    fb.goto(next, head);
    fb.ret(exit, copy_local(s));
    pb.add(fb.finish());
    let p = pb.finish();
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], s), "s stays in [0, 1000)");
    assert_eq!(call(&p, &[]), call(&q, &[]));
}

#[test]
fn to_int32_of_a_constant_is_folded() {
    let mut pb = ProgramBuilder::new();
    let to_int32 = pb.ext(TO_INT32, &[Ty::F64], Ty::I32, false);
    let mut fb = FuncBuilder::export("f", &[], Ty::I32);
    let r = fb.local(Ty::I32);
    let b0 = fb.block();
    let b1 = fb.call(
        b0,
        Callee::Extern(to_int32),
        vec![float(4294967301.5, Ty::F64)],
        Some(r),
    );
    fb.ret(b1, copy_local(r));
    pb.add(fb.finish());
    let p = pb.finish();
    let q = narrowed(&p);
    assert_eq!(count_calls(&q.funcs[0]), 0);
    assert_eq!(call(&q, &[]).expect("runs") as u32, 5);
}

#[test]
fn a_counter_added_to_a_64_bit_sum_is_64_bit() {
    // `for (k = 0; k < 1000; k++) s = (s + k) % 3000000007`: `s` needs 64 bits, so `k`, which
    // is only added to it, takes 64 bits too (no conversion per iteration).
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[], Ty::F64);
    let (s, k, c, t, r, u) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
    );
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.assign(entry, k, Rvalue::Use(float(0.0, Ty::F64)));
    fb.goto(entry, head);
    fb.assign(
        head,
        c,
        bin(BinOp::Lt, copy_local(k), float(1000.0, Ty::F64)),
    );
    fb.branch(head, c, body, exit);
    fb.assign(body, t, bin(BinOp::Add, copy_local(s), copy_local(k)));
    fb.assign(
        body,
        r,
        bin(BinOp::Rem, copy_local(t), float(3000000007.0, Ty::F64)),
    );
    fb.assign(body, s, Rvalue::Use(copy_local(r)));
    fb.assign(body, u, bin(BinOp::Add, copy_local(k), float(1.0, Ty::F64)));
    fb.assign(body, k, Rvalue::Use(copy_local(u)));
    fb.goto(body, head);
    fb.ret(exit, copy_local(s));
    pb.add(fb.finish());
    let p = pb.finish();
    let q = narrowed(&p);
    let f = &q.funcs[0];
    assert_eq!(local_ty_count(f, Ty::I32), 0, "{f:?}");
    assert_eq!(call(&p, &[]), call(&q, &[]));
}
