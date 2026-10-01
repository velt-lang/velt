//! Focused async lowering tests: cancelling futures before start / mid-await / inside an
//! embedded child (the drop function frees exactly what is live — the interpreter fails on
//! leaks and double frees), dropping unpolled promise values, awaits inside loops with
//! `try`/`finally`, `continue`/`break` (and an await inside `finally`), and 10k spawned tasks.

use velt_sema::hir::{BinOp as B, DefId, Intrinsic as I, PassMode, Program, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::builder_m3::*;
use super::interp::{poll_then_drop, Arg};
use super::programs_m3::{count_to, delayed};
use super::run;

/// `holder(s, moveIt)`: owns `s`, two derived strings (one conditionally moved away before the
/// await, so it has a drop flag) and a sleep future while suspended. `outer(s)` embeds a
/// `holder` child. `main` creates one promise of each and drops them unpolled.
fn cancel_program() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let pv = pb.promise(t.unit);
    let take = {
        let mut f = FB::new("take", t.unit);
        f.param("x", t.str, PassMode::Owned);
        pb.add_fn(f.build(vec![]))
    };
    let holder = {
        let mut f = FB::new("holder", pv);
        let s_ = f.param("s", t.str, PassMode::Owned);
        let move_it = f.param("moveIt", t.bool, PassMode::Copy);
        let a = f.local("a", t.str);
        let b = f.local("b", t.str);
        let body = vec![
            let_(a, concat(f.bw(s_), s("x", t), t)),
            let_(b, concat(s("tmp", t), f.bw(s_), t)),
            if_(
                f.cp(move_it),
                vec![se(call(take, vec![f.mv(a)], t.unit))],
                None,
            ),
            se(await_(sleep(int(5, t.i64), pv), t.unit)),
            se(print(vec![f.bw(s_), f.bw(b)], t)),
        ];
        pb.add_fn(f.build_async(body))
    };
    let outer = {
        let mut f = FB::new("outer", pv);
        let s_ = f.param("s", t.str, PassMode::Owned);
        let o = f.local("o", t.str);
        let child = call(
            holder,
            vec![intr(I::Clone, vec![f.bw(s_)], t.str), boolean(false, t)],
            pv,
        );
        let body = vec![
            let_(o, concat(f.bw(s_), s("!", t), t)),
            se(await_(child, t.unit)),
            se(print(vec![f.bw(o)], t)),
        ];
        pb.add_fn(f.build_async(body))
    };
    let f = FB::new("main", t.unit);
    let body = vec![
        se(call(holder, vec![s("abc", t), boolean(true, t)], pv)),
        se(call(outer, vec![s("zz", t)], pv)),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn unpolled_promises_drop_their_arguments() {
    let out = run(&cancel_program());
    assert_eq!(out.stdout, "");
}

#[test]
fn cancel_before_start_and_mid_await() {
    let p = super::lower_ok(&cancel_program());
    for move_it in [0, 1] {
        for polls in [0, 1, 3] {
            let args = [Arg::Str("abc"), Arg::Int(move_it)];
            let r = poll_then_drop(&p, "_V6holder", &args, polls);
            assert!(!r.ready, "polls={polls}");
            assert_eq!(r.live_allocs, 0, "leak: moveIt={move_it} polls={polls}");
        }
    }
    let done = poll_then_drop(&p, "_V6holder", &[Arg::Str("abc"), Arg::Int(1)], 8);
    assert!(done.ready);
    assert_eq!(done.stdout, "abc tmpabc\n");
    assert_eq!(done.live_allocs, 0);
}

#[test]
fn cancel_parent_drops_embedded_child() {
    let p = super::lower_ok(&cancel_program());
    for polls in [0, 1, 2] {
        let r = poll_then_drop(&p, "_V5outer", &[Arg::Str("zz")], polls);
        assert!(!r.ready);
        assert_eq!(r.live_allocs, 0, "leak after {polls} polls");
    }
    let done = poll_then_drop(&p, "_V5outer", &[Arg::Str("zz")], 10);
    assert!(done.ready);
    assert_eq!(done.stdout, "zz tmpzz\nzz!\n");
    assert_eq!(done.live_allocs, 0);
}

/// ```text
/// async function loopy(): Promise<i64> {
///   let total = 0;
///   for (let i = 0; i < 5; i++) {
///     const tag = `i${i}`;                       // owned, alive across every await below
///     try {
///       if (i == 3) continue;
///       total += await delayed(1, i);
///       if (i == 4) break;
///     } finally { await sleep(1); total += 100; console.log(tag); }
///   }
///   return total;
/// }
/// ```
fn loopy(pb: &mut PB, delayed: DefId) -> DefId {
    let t = pb.t;
    let (pi, pv) = (pb.promise(t.i64), pb.promise(t.unit));
    let mut f = FB::new("loopy", pi);
    let total = f.local("total", t.i64);
    let i = f.local("i", t.i64);
    let tag = f.local("tag", t.str);
    let body_try = vec![
        if_(
            cmp(B::Eq, f.cp(i), int(3, t.i64), t),
            vec![cont(None)],
            None,
        ),
        se(cassign(
            B::Add,
            f.bm(total),
            await_(call(delayed, vec![int(1, t.i64), f.cp(i)], pi), t.i64),
            t,
        )),
        if_(cmp(B::Eq, f.cp(i), int(4, t.i64), t), vec![brk(None)], None),
    ];
    let fin = vec![
        se(await_(sleep(int(1, t.i64), pv), t.unit)),
        se(cassign(B::Add, f.bm(total), int(100, t.i64), t)),
        se(print(vec![f.bw(tag)], t)),
    ];
    let body = vec![
        let_(total, int(0, t.i64)),
        count_to(
            &f,
            i,
            5,
            t,
            vec![
                let_(tag, concat(s("i", t), to_s(f.cp(i), t), t)),
                try_(body_try, None, Some(fin)),
            ],
        ),
        ret(Some(f.cp(total))),
    ];
    pb.add_fn(f.build_async(body))
}

#[test]
fn awaits_in_loops_try_finally_break_continue() {
    let mut pb = PB::new();
    let t = pb.t;
    let d = delayed(&mut pb);
    let l = loopy(&mut pb, d);
    let (pi, pv) = (pb.promise(t.i64), pb.promise(t.unit));
    let f = FB::new("main", pv);
    let body = vec![se(print(vec![await_(call(l, vec![], pi), t.i64)], t))];
    pb.add_main(f.build_async(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "i0\ni1\ni2\ni3\ni4\n507\n");
}

#[test]
fn ten_thousand_tasks() {
    let mut pb = PB::new();
    let t = pb.t;
    let (pi, pv) = (pb.promise(t.i64), pb.promise(t.unit));
    let (api, ai) = (pb.arr(pi), pb.arr(t.i64));
    let pai = pb.promise(ai);
    let job = {
        let mut f = FB::new("job", pi);
        let i = f.param("i", t.i64, PassMode::Copy);
        let body = vec![
            se(await_(intr(I::YieldNow, vec![], pv), t.unit)),
            ret(Some(bin(B::Mul, f.cp(i), int(2, t.i64)))),
        ];
        pb.add_fn(f.build_async(body))
    };
    let mut f = FB::new("main", pv);
    let hs = f.local("hs", api);
    let i = f.local("i", t.i64);
    let rs = f.local("rs", ai);
    let sum = f.local("sum", t.i64);
    let x = f.local("x", t.i64);
    let push = intr(
        I::ArrayPush,
        vec![f.bm(hs), spawn(call(job, vec![f.cp(i)], pi), pi)],
        t.unit,
    );
    let body = vec![
        let_(hs, array(vec![], api)),
        count_to(&f, i, 10_000, t, vec![se(push)]),
        let_(rs, await_(promise_all(f.mv(hs), pai), ai)),
        let_(sum, int(0, t.i64)),
        for_of(
            pbind(x, U::Copy, t.i64),
            f.bw(rs),
            vec![se(cassign(B::Add, f.bm(sum), f.cp(x), t))],
        ),
        se(print(
            vec![intr(I::ArrayLen, vec![f.bw(rs)], t.usize), f.cp(sum)],
            t,
        )),
    ];
    pb.add_main(f.build_async(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "10000 99990000\n");
}
