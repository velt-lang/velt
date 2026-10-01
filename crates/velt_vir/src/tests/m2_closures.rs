//! `tests/golden/m2/closures.vlt` hand-lowered to HIR: stack environments (borrowed captures),
//! heap environments (moved captures), named functions as values, generic higher-order
//! functions (the prelude's `forEach`/`map`/`filter`/`reduce`, written here as generic fns).

use velt_sema::hir::{BinOp as B, Intrinsic, PassMode, Program};

use super::builder::*;
use super::builder_m2::*;
use super::builder_prelude::prelude;
use super::{m2_golden, run};

pub(super) fn closures() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let pre = prelude(&mut pb);
    let ia = pb.arr(t.i64);
    let i2i = pb.fn_ty(vec![t.i64], t.i64);
    let v2i = pb.fn_ty(vec![], t.i64);
    let v2s = pb.fn_ty(vec![], t.str);
    let apply = {
        let mut f = FB::new("apply", t.i64);
        let cb = f.param("f", i2i, PassMode::Borrow);
        let v = f.param("v", t.i64, PassMode::Copy);
        let body = vec![ret(Some(call_ptr(f.bw(cb), vec![f.cp(v)], t.i64)))];
        pb.add_fn(f.build(body))
    };
    let make_counter = {
        let mut f = FB::new("makeCounter", v2i);
        let n = f.local("n", t.i64);
        let c = closure_def(
            &mut pb,
            "makeCounter::{closure#0}",
            t.i64,
            &[(n, t.i64, PassMode::Owned)],
            &[],
            |f, caps, _| {
                vec![
                    se(cassign(B::Add, f.bm(caps[0]), int(1, t.i64), t)),
                    ret(Some(f.cp(caps[0]))),
                ]
            },
        );
        let body = vec![let_(n, int(0, t.i64)), ret(Some(closure(c, v2i)))];
        pb.add_fn(f.build(body))
    };
    let make_adder = {
        let mut f = FB::new("makeAdder", i2i);
        let k = f.param("k", t.i64, PassMode::Copy);
        let c = closure_def(
            &mut pb,
            "makeAdder::{closure#0}",
            t.i64,
            &[(k, t.i64, PassMode::Copy)],
            &[("x", t.i64)],
            |f, caps, ps| vec![ret(Some(bin(B::Add, f.cp(ps[0]), f.cp(caps[0]))))],
        );
        pb.add_fn(f.build(vec![ret(Some(closure(c, i2i)))]))
    };
    let mut f = FB::new("main", t.unit);
    let total = f.local("total", t.i64);
    let xs = f.local("xs", ia);
    let factor = f.local("factor", t.i64);
    let c = f.local("c", v2i);
    let add5 = f.local("add5", i2i);
    let name = f.local("name", t.str);
    let greet = f.local("greet", v2s);
    let unit_fn = pb.fn_ty(vec![t.i64], t.unit);
    let bool_fn = pb.fn_ty(vec![t.i64], t.bool);
    let acc_fn = pb.fn_ty(vec![t.i64, t.i64], t.i64);
    let add_total = closure_def(
        &mut pb,
        "main::{closure#0}",
        t.unit,
        &[(total, t.i64, PassMode::BorrowMut)],
        &[("x", t.i64)],
        |f, caps, ps| vec![se(cassign(B::Add, f.bm(caps[0]), f.cp(ps[0]), t))],
    );
    let times_factor = closure_def(
        &mut pb,
        "main::{closure#1}",
        t.i64,
        &[(factor, t.i64, PassMode::Borrow)],
        &[("x", t.i64)],
        |f, caps, ps| vec![ret(Some(bin(B::Mul, f.cp(ps[0]), f.cp(caps[0]))))],
    );
    let greet_c = closure_def(
        &mut pb,
        "main::{closure#2}",
        t.str,
        &[(name, t.str, PassMode::Owned)],
        &[],
        |f, caps, _| vec![ret(Some(concat(s("hi ", t), f.bw(caps[0]), t)))],
    );
    let square = closure_def(
        &mut pb,
        "main::{closure#3}",
        t.i64,
        &[],
        &[("x", t.i64)],
        |f, _, ps| vec![ret(Some(bin(B::Mul, f.cp(ps[0]), f.cp(ps[0]))))],
    );
    let gt4 = closure_def(
        &mut pb,
        "main::{closure#4}",
        t.bool,
        &[],
        &[("x", t.i64)],
        |f, _, ps| vec![ret(Some(cmp(B::Gt, f.cp(ps[0]), int(4, t.i64), t)))],
    );
    let sum = closure_def(
        &mut pb,
        "main::{closure#5}",
        t.i64,
        &[],
        &[("acc", t.i64), ("x", t.i64)],
        |f, _, ps| vec![ret(Some(bin(B::Add, f.cp(ps[0]), f.cp(ps[1]))))],
    );
    let i = t.i64;
    let mapped = call_g(
        pre.map,
        vec![i, i],
        vec![f.bw(xs), closure(square, i2i)],
        ia,
    );
    let filtered = call_g(pre.filter, vec![i], vec![mapped, closure(gt4, bool_fn)], ia);
    let body = vec![
        let_(total, int(0, i)),
        let_(xs, array((1..=4).map(|v| int(v, i)).collect(), ia)),
        se(call_g(
            pre.for_each,
            vec![i],
            vec![f.bw(xs), closure(add_total, unit_fn)],
            t.unit,
        )),
        se(print(vec![f.cp(total)], t)),
        let_(factor, int(10, i)),
        se(print(
            vec![call(apply, vec![closure(times_factor, i2i), int(5, i)], i)],
            t,
        )),
        let_(c, call(make_counter, vec![], v2i)),
        se(call_ptr(f.bw(c), vec![], i)),
        se(call_ptr(f.bw(c), vec![], i)),
        se(print(vec![call_ptr(f.bw(c), vec![], i)], t)),
        let_(add5, call(make_adder, vec![int(5, i)], i2i)),
        se(print(
            vec![
                call_ptr(f.bw(add5), vec![int(1, i)], i),
                call(apply, vec![f.bw(add5), int(10, i)], i),
            ],
            t,
        )),
        let_(name, s("velt", t)),
        let_(greet, closure(greet_c, v2s)),
        se(print(vec![call_ptr(f.bw(greet), vec![], t.str)], t)),
        se(print(
            vec![intr(Intrinsic::ArrayLen, vec![filtered], t.usize)],
            t,
        )),
        se(print(
            vec![call_g(
                pre.reduce,
                vec![i, i],
                vec![f.bw(xs), closure(sum, acc_fn), int(100, i)],
                i,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_closures() {
    let out = run(&closures());
    assert_eq!(out.stdout, m2_golden("closures"));
}

/// Named functions as values (env-ignoring thunk), cloned heap closures, and a closure that
/// owns a string (its env frees it).
#[test]
fn fn_values_and_closure_clone() {
    let mut pb = PB::new();
    let t = pb.t;
    let s2s = pb.fn_ty(vec![t.str], t.str);
    let shout = {
        let mut f = FB::new("shout", t.str);
        let x = f.param("x", t.str, PassMode::Owned);
        let body = vec![ret(Some(concat(f.bw(x), s("!", t), t)))];
        pb.add_fn(f.build(body))
    };
    let mut f = FB::new("main", t.unit);
    let g = f.local("g", s2s);
    let suffix = f.local("suffix", t.str);
    let h = f.local("h", s2s);
    let h2 = f.local("h2", s2s);
    let add = closure_def(
        &mut pb,
        "main::{closure#0}",
        t.str,
        &[(suffix, t.str, PassMode::Owned)],
        &[("x", t.str)],
        |f, caps, ps| vec![ret(Some(concat(f.bw(ps[0]), f.bw(caps[0]), t)))],
    );

    let body = vec![
        let_(g, fn_ref(shout, s2s)),
        se(print(
            vec![call_ptr(
                f.bw(g),
                vec![concat(s("a", t), s("b", t), t)],
                t.str,
            )],
            t,
        )),
        let_(suffix, concat(s("-", t), s("x", t), t)),
        let_(h, closure(add, s2s)),
        let_(h2, intr(Intrinsic::Clone, vec![f.bw(h)], s2s)),
        se(print(
            vec![
                call_ptr(f.bw(h), vec![s("q", t)], t.str),
                call_ptr(f.bw(h2), vec![s("r", t)], t.str),
            ],
            t,
        )),
        se(print(vec![f.bw(h)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "ab!\nq-x r-x\n[Function (anonymous)]\n");
}

/// `function twice(f, x) { const g = f.clone(); return g(x) + f(x); }` called as
/// `twice((x) => x * k, 2)`: a closure literal passed by borrow keeps its Copy captures in a
/// frame env (no allocation in `main`); cloning it still yields an owned heap env.
#[test]
fn borrowed_closure_literal_env_is_on_the_stack() {
    let mut pb = PB::new();
    let t = pb.t;
    let i2i = pb.fn_ty(vec![t.i64], t.i64);
    let twice = {
        let mut f = FB::new("twice", t.i64);
        let cb = f.param("f", i2i, PassMode::Borrow);
        let x = f.param("x", t.i64, PassMode::Copy);
        let g = f.local("g", i2i);
        let body = vec![
            let_(g, intr(Intrinsic::Clone, vec![f.bw(cb)], i2i)),
            ret(Some(bin(
                B::Add,
                call_ptr(f.bw(g), vec![f.cp(x)], t.i64),
                call_ptr(f.bw(cb), vec![f.cp(x)], t.i64),
            ))),
        ];
        pb.add_fn(f.build(body))
    };
    let mut f = FB::new("main", t.unit);
    let k = f.local("k", t.i64);
    let times_k = closure_def(
        &mut pb,
        "main::{closure#0}",
        t.i64,
        &[(k, t.i64, PassMode::Copy)],
        &[("x", t.i64)],
        |f, caps, ps| vec![ret(Some(bin(B::Mul, f.cp(ps[0]), f.cp(caps[0]))))],
    );
    let body = vec![
        let_(k, int(10, t.i64)),
        se(print(
            vec![call(
                twice,
                vec![closure(times_k, i2i), int(2, t.i64)],
                t.i64,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let p = pb.finish();
    let v = super::lower_ok(&p);
    let main = v
        .funcs
        .iter()
        .find(|f| f.symbol == "_V4main")
        .expect("main");
    let allocs = main.blocks.iter().filter(|b| {
        matches!(&b.term, crate::vir::Terminator::Call { callee: crate::vir::Callee::Extern(e), .. }
            if v.externs[e.0 as usize].symbol == "velt_rt_alloc")
    });
    assert_eq!(allocs.count(), 0, "{v}");
    let out = run(&p);
    assert_eq!(out.stdout, "40\n");
}
