//! Control-flow programs: loops, recursion (direct and mutual), call chains, called-once
//! helpers, dead branches, unit callees and noreturn externs.

use super::*;
use velt_vir::vir::Ty::*;

pub fn cases() -> Vec<Case> {
    vec![
        fib(),
        hot_loop(),
        nested_loops(),
        even_odd(),
        gcd_loop(),
        call_chain(),
        called_once_big(),
        dead_branch(),
        unit_callee(),
        div_guard(),
    ]
}

/// `while (i < n) { body }`: returns (head, body, exit) blocks; `b` jumps to head.
fn counted_loop(
    fb: &mut FuncBuilder,
    b: BlockId,
    i: Local,
    n: Operand,
) -> (BlockId, BlockId, BlockId) {
    let c = fb.local(Bool);
    let (head, body, exit) = (fb.block(), fb.block(), fb.block());
    fb.assign(b, i, Rvalue::Use(int(0, I64)));
    fb.goto(b, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), n));
    fb.branch(head, c, body, exit);
    (head, body, exit)
}

/// `i = i + 1; goto head` at the end of `b`.
fn step(fb: &mut FuncBuilder, b: BlockId, i: Local, head: BlockId) {
    fb.assign(b, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(b, head);
}

/// Recursive and iterative fib; main prints both for a few n.
fn fib() -> Case {
    let mut env = Env::new();
    let rec = env.pb.reserve();
    let mut fb = FuncBuilder::internal("fib", &[I64], I64);
    let n = fb.param(0);
    let (c, a, r1, r2, s) = (
        fb.local(Bool),
        fb.local(I64),
        fb.local(I64),
        fb.local(I64),
        fb.local(I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Lt, copy_local(n), int(2, I64)));
    fb.branch(b0, c, b1, b2);
    fb.ret(b1, copy_local(n));
    fb.assign(b2, a, bin(BinOp::Sub, copy_local(n), int(1, I64)));
    let b3 = fb.call(b2, Callee::Func(rec), vec![copy_local(a)], Some(r1));
    fb.assign(b3, a, bin(BinOp::Sub, copy_local(n), int(2, I64)));
    let b4 = fb.call(b3, Callee::Func(rec), vec![copy_local(a)], Some(r2));
    fb.assign(b4, s, bin(BinOp::Add, copy_local(r1), copy_local(r2)));
    fb.ret(b4, copy_local(s));
    env.pb.set(rec, fb.finish());

    let mut fb = FuncBuilder::internal("fib_iter", &[I64], I64);
    let n = fb.param(0);
    let (x, y, t, i) = (fb.local(I64), fb.local(I64), fb.local(I64), fb.local(I64));
    let b0 = fb.block();
    fb.assign(b0, x, Rvalue::Use(int(0, I64)));
    fb.assign(b0, y, Rvalue::Use(int(1, I64)));
    let (head, body, exit) = counted_loop(&mut fb, b0, i, copy_local(n));
    fb.assign(body, t, bin(BinOp::Add, copy_local(x), copy_local(y)));
    fb.assign(body, x, Rvalue::Use(copy_local(y)));
    fb.assign(body, y, Rvalue::Use(copy_local(t)));
    step(&mut fb, body, i, head);
    fb.ret(exit, copy_local(x));
    let iter = env.pb.add(fb.finish());

    let (mut fb, mut b) = main_fn();
    for n in [0, 1, 2, 10, 20] {
        for f in [rec, iter] {
            let r = fb.local(I64);
            b = fb.call(b, Callee::Func(f), vec![int(n, I64)], Some(r));
            b = env.print(&mut fb, b, copy_local(r), I64);
        }
    }
    fb.ret(b, int(0, I32));
    env.pb.add(fb.finish());
    env.case("fib", "velt_main", vec![vec![]])
}

/// `mix(a, b) = a + (b ^ 5)` called in a loop: the benchmark shape (a tiny helper whose
/// work is a one-cycle dependency chain, so call overhead is what shows).
pub fn hot_loop_program(n: i64) -> Program {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("mix", &[I64, I64], I64);
    let (a, bb) = (fb.param(0), fb.param(1));
    let t = fb.local(I64);
    let b = fb.block();
    fb.assign(b, t, bin(BinOp::BitXor, copy_local(bb), int(5, I64)));
    fb.assign(b, t, bin(BinOp::Add, copy_local(a), copy_local(t)));
    fb.ret(b, copy_local(t));
    let mix = env.pb.add(fb.finish());

    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (acc, i, r) = (fb.local(I64), fb.local(I64), fb.local(I32));
    let b0 = fb.block();
    fb.assign(b0, acc, Rvalue::Use(int(7, I64)));
    let (head, body, exit) = counted_loop(&mut fb, b0, i, int(n as i128, I64));
    let next = fb.call(
        body,
        Callee::Func(mix),
        vec![copy_local(acc), copy_local(i)],
        Some(acc),
    );
    step(&mut fb, next, i, head);
    fb.assign(
        exit,
        acc,
        bin(BinOp::BitAnd, copy_local(acc), int(0x7F, I64)),
    );
    fb.assign(exit, r, Rvalue::Cast(copy_local(acc), I32));
    fb.ret(exit, copy_local(r));
    env.pb.add(fb.finish());
    env.pb.finish()
}

fn hot_loop() -> Case {
    Case {
        name: "hot_loop",
        program: hot_loop_program(1000),
        runs: vec![("velt_main", vec![])],
    }
}

/// Sum of `i * j` over `i, j < n`, with the inner loop in a helper.
fn nested_loops() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("row", &[I64, I64], I64);
    let (i, n) = (fb.param(0), fb.param(1));
    let (j, s, t) = (fb.local(I64), fb.local(I64), fb.local(I64));
    let b0 = fb.block();
    fb.assign(b0, s, Rvalue::Use(int(0, I64)));
    let (head, body, exit) = counted_loop(&mut fb, b0, j, copy_local(n));
    fb.assign(body, t, bin(BinOp::Mul, copy_local(i), copy_local(j)));
    fb.assign(body, s, bin(BinOp::Add, copy_local(s), copy_local(t)));
    step(&mut fb, body, j, head);
    fb.ret(exit, copy_local(s));
    let row = env.pb.add(fb.finish());
    let mut fb = FuncBuilder::export("grid", &[I64], I64);
    let n = fb.param(0);
    let (i, s, r) = (fb.local(I64), fb.local(I64), fb.local(I64));
    let b0 = fb.block();
    fb.assign(b0, s, Rvalue::Use(int(0, I64)));
    let (head, body, exit) = counted_loop(&mut fb, b0, i, copy_local(n));
    let next = fb.call(
        body,
        Callee::Func(row),
        vec![copy_local(i), copy_local(n)],
        Some(r),
    );
    fb.assign(next, s, bin(BinOp::Add, copy_local(s), copy_local(r)));
    step(&mut fb, next, i, head);
    fb.ret(exit, copy_local(s));
    env.pb.add(fb.finish());
    let inputs = [0, 1, 7, 30].iter().map(|&v| vec![arg(v)]).collect();
    env.case("nested_loops", "grid", inputs)
}

/// Mutual recursion: `even(n) = n == 0 || odd(n - 1)`, `odd(n) = n != 0 && even(n - 1)`.
fn even_odd() -> Case {
    let mut env = Env::new();
    let (even, odd) = (env.pb.reserve(), env.pb.reserve());
    for (id, other, base) in [(even, odd, true), (odd, even, false)] {
        let mut fb = FuncBuilder::internal(if base { "even" } else { "odd" }, &[I64], Bool);
        let n = fb.param(0);
        let (c, m, r) = (fb.local(Bool), fb.local(I64), fb.local(Bool));
        let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
        fb.assign(b0, c, bin(BinOp::Eq, copy_local(n), int(0, I64)));
        fb.branch(b0, c, b1, b2);
        fb.ret(b1, boolean(base));
        fb.assign(b2, m, bin(BinOp::Sub, copy_local(n), int(1, I64)));
        let b3 = fb.call(b2, Callee::Func(other), vec![copy_local(m)], Some(r));
        fb.ret(b3, copy_local(r));
        env.pb.set(id, fb.finish());
    }
    let mut fb = FuncBuilder::export("parity", &[I64], I64);
    let (r, w) = (fb.local(Bool), fb.local(I64));
    let b = fb.block();
    let b = fb.call(
        b,
        Callee::Func(even),
        vec![copy_local(fb.param(0))],
        Some(r),
    );
    fb.assign(b, w, Rvalue::Cast(copy_local(r), I64));
    fb.ret(b, copy_local(w));
    env.pb.add(fb.finish());
    let inputs = [0, 1, 6, 101].iter().map(|&v| vec![arg(v)]).collect();
    env.case("even_odd", "parity", inputs)
}

/// `gcd(a, b)` mutates its params in a loop; main sums gcd(i, 36) for i < n.
fn gcd_loop() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("gcd", &[I64, I64], I64);
    let (a, b) = (fb.param(0), fb.param(1));
    let (c, t) = (fb.local(Bool), fb.local(I64));
    let (b0, body, exit) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Ne, copy_local(b), int(0, I64)));
    fb.branch(b0, c, body, exit);
    fb.assign(body, t, bin(BinOp::Rem, copy_local(a), copy_local(b)));
    fb.assign(body, a, Rvalue::Use(copy_local(b)));
    fb.assign(body, b, Rvalue::Use(copy_local(t)));
    fb.goto(body, b0);
    fb.ret(exit, copy_local(a));
    let gcd = env.pb.add(fb.finish());
    let mut fb = FuncBuilder::export("gcd_sum", &[I64], I64);
    let n = fb.param(0);
    let (i, s, r) = (fb.local(I64), fb.local(I64), fb.local(I64));
    let b0 = fb.block();
    fb.assign(b0, s, Rvalue::Use(int(0, I64)));
    let (head, body, exit) = counted_loop(&mut fb, b0, i, copy_local(n));
    let next = fb.call(
        body,
        Callee::Func(gcd),
        vec![copy_local(i), int(36, I64)],
        Some(r),
    );
    fb.assign(next, s, bin(BinOp::Add, copy_local(s), copy_local(r)));
    step(&mut fb, next, i, head);
    fb.ret(exit, copy_local(s));
    env.pb.add(fb.finish());
    let inputs = [0, 1, 50].iter().map(|&v| vec![arg(v)]).collect();
    env.case("gcd_loop", "gcd_sum", inputs)
}

/// f1(x) = f2(x + 1) * 2, … f6(x) = x - 3: a chain of small functions.
fn call_chain() -> Case {
    let mut env = Env::new();
    let ids: Vec<FuncId> = (0..6).map(|_| env.pb.reserve()).collect();
    for k in 0..6 {
        let mut fb = FuncBuilder::internal(&format!("f{k}"), &[I64], I64);
        let x = fb.param(0);
        let (y, r) = (fb.local(I64), fb.local(I64));
        let b = fb.block();
        if k == 5 {
            fb.assign(b, r, bin(BinOp::Sub, copy_local(x), int(3, I64)));
            fb.ret(b, copy_local(r));
        } else {
            fb.assign(
                b,
                y,
                bin(BinOp::Add, copy_local(x), int(k as i128 + 1, I64)),
            );
            let b = fb.call(b, Callee::Func(ids[k + 1]), vec![copy_local(y)], Some(r));
            fb.assign(b, r, bin(BinOp::Mul, copy_local(r), int(2, I64)));
            fb.ret(b, copy_local(r));
        }
        env.pb.set(ids[k], fb.finish());
    }
    let mut fb = FuncBuilder::export("chain", &[I64], I64);
    let r = fb.local(I64);
    let b = fb.block();
    let b = fb.call(
        b,
        Callee::Func(ids[0]),
        vec![copy_local(fb.param(0))],
        Some(r),
    );
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    let (mut fb, b) = main_fn();
    let r = fb.local(I64);
    let b = fb.call(b, Callee::Func(ids[0]), vec![int(10, I64)], Some(r));
    let b = env.print(&mut fb, b, copy_local(r), I64);
    fb.ret(b, int(0, I32));
    env.pb.add(fb.finish());
    env.case("call_chain", "chain", vec![vec![arg(0)], vec![arg(-100)]])
        .with("velt_main", vec![vec![]])
}

/// A large helper (> the small-callee threshold) called once: inlined anyway.
fn called_once_big() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("big", &[I64], I64);
    let x = fb.param(0);
    let (i, acc) = (fb.local(I64), fb.local(I64));
    let b0 = fb.block();
    fb.assign(b0, acc, Rvalue::Use(copy_local(x)));
    let (head, body, exit) = counted_loop(&mut fb, b0, i, int(5, I64));
    for k in 0..60 {
        let op = [BinOp::Add, BinOp::Mul, BinOp::BitXor][k % 3];
        fb.assign(body, acc, bin(op, copy_local(acc), int(k as i128 + 3, I64)));
    }
    step(&mut fb, body, i, head);
    fb.ret(exit, copy_local(acc));
    let big = env.pb.add(fb.finish());
    let mut fb = FuncBuilder::export("once", &[I64], I64);
    let r = fb.local(I64);
    let b = fb.block();
    let b = fb.call(b, Callee::Func(big), vec![copy_local(fb.param(0))], Some(r));
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    env.case("called_once_big", "once", vec![vec![arg(1)], vec![arg(-9)]])
}

/// `if (2 > 3) print(111); print(222)` plus a runtime-dependent branch.
fn dead_branch() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::export("dead", &[I64], I64);
    let x = fb.param(0);
    let (c, d) = (fb.local(Bool), fb.local(Bool));
    let (b0, yes, join, rt_yes, end) = (fb.block(), fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Gt, int(2, I64), int(3, I64)));
    fb.branch(b0, c, yes, join);
    let after = env.print(&mut fb, yes, int(111, I64), I64);
    fb.goto(after, join);
    let j = env.print(&mut fb, join, int(222, I64), I64);
    fb.assign(j, d, bin(BinOp::Gt, copy_local(x), int(0, I64)));
    fb.branch(j, d, rt_yes, end);
    let after = env.print(&mut fb, rt_yes, copy_local(x), I64);
    fb.goto(after, end);
    fb.ret(end, copy_local(x));
    env.pb.add(fb.finish());
    env.case("dead_branch", "dead", vec![vec![arg(5)], vec![arg(-5)]])
}

/// `log(v)` returns Unit and calls an extern; called in a loop and directly.
fn unit_callee() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("log", &[I64], Unit);
    let v = fb.param(0);
    let b = fb.block();
    let b = env.show(&mut fb, b, bin(BinOp::Mul, copy_local(v), int(2, I64)), I64);
    fb.ret(b, unit());
    let log = env.pb.add(fb.finish());
    let (mut fb, b0) = main_fn();
    let i = fb.local(I64);
    let (head, body, exit) = counted_loop(&mut fb, b0, i, int(4, I64));
    let next = fb.call(body, Callee::Func(log), vec![copy_local(i)], None);
    step(&mut fb, next, i, head);
    let b = fb.call(exit, Callee::Func(log), vec![int(-1, I64)], None);
    fb.ret(b, int(3, I32));
    env.pb.add(fb.finish());
    env.case("unit_callee", "velt_main", vec![vec![]])
}

/// `div(a, b)` panics (noreturn extern + `Unreachable`) on b == 0.
fn div_guard() -> Case {
    let mut env = Env::new();
    let msg = env.pb.stat(b"division by zero", 1);
    let mut fb = FuncBuilder::internal("div", &[I64, I64], I64);
    let (a, b) = (fb.param(0), fb.param(1));
    let (z, q) = (fb.local(Bool), fb.local(I64));
    let (b0, bad, ok) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, z, bin(BinOp::Eq, copy_local(b), int(0, I64)));
    fb.branch(b0, z, bad, ok);
    let dead = fb.call(
        bad,
        Callee::Extern(env.panic),
        vec![Operand::Const(Const::Static(msg), Ptr)],
        None,
    );
    fb.term(dead, Terminator::Unreachable);
    fb.assign(ok, q, bin(BinOp::Div, copy_local(a), copy_local(b)));
    fb.ret(ok, copy_local(q));
    let div = env.pb.add(fb.finish());
    let mut fb = FuncBuilder::export("divs", &[I64], I64);
    let d = fb.param(0);
    let (r, s) = (fb.local(I64), fb.local(I64));
    let b = fb.block();
    let b = fb.call(
        b,
        Callee::Func(div),
        vec![int(10, I64), int(2, I64)],
        Some(r),
    );
    let b = env.print(&mut fb, b, copy_local(r), I64);
    let b = fb.call(
        b,
        Callee::Func(div),
        vec![int(7, I64), copy_local(d)],
        Some(s),
    );
    fb.ret(b, copy_local(s));
    env.pb.add(fb.finish());
    env.case("div_guard", "divs", vec![vec![arg(3)], vec![arg(0)]])
}
