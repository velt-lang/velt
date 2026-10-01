//! Async state size and `Promise.all` performance lowering: locals of disjoint suspension
//! points share state bytes (regression test on the layout sizes), and `await Promise.all([…])`
//! of an array literal runs inline — embedded children polled in place, no `velt_rt_all` —
//! with exact cleanup when cancelled halfway.

use velt_sema::hir::{Intrinsic as I, PassMode, Program, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::builder_m3::*;
use super::interp::poll_then_drop;
use super::programs_m3::delayed;
use super::{lower_ok, run};
use crate::vir;

fn state_size(v: &vir::Program, name: &str) -> u32 {
    let full = format!("{name} state");
    v.aggs
        .iter()
        .find(|a| a.name == full)
        .unwrap_or_else(|| panic!("no layout {full}"))
        .size
}

/// ```text
/// async function seq(): Promise<i64> {
///   const a = await delayed(1, 1);
///   const b = await delayed(2, 41);
///   return a + b;
/// }
/// async function phases() {
///   { const x = "a" + "b"; await sleep(1); console.log(x); }
///   { const y = "c" + "d"; await sleep(1); console.log(y); }
/// }
/// async function main() { console.log(await seq()); await phases(); }
/// ```
fn sequential_program() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let delayed = delayed(&mut pb);
    let (pi, pv) = (pb.promise(t.i64), pb.promise(t.unit));
    let seq = {
        let mut f = FB::new("seq", pi);
        let a = f.local("a", t.i64);
        let b = f.local("b", t.i64);
        let d = |ms, v| await_(call(delayed, vec![int(ms, t.i64), v], pi), t.i64);
        let body = vec![
            let_(a, d(1, int(1, t.i64))),
            let_(b, d(2, int(41, t.i64))),
            ret(Some(bin(velt_sema::hir::BinOp::Add, f.cp(a), f.cp(b)))),
        ];
        pb.add_fn(f.build_async(body))
    };
    let phases = {
        let mut f = FB::new("phases", pv);
        let x = f.local("x", t.str);
        let y = f.local("y", t.str);
        let phase = |f: &FB, l, a: &str, b: &str| {
            sblock(vec![
                let_(l, concat(s(a, t), s(b, t), t)),
                se(await_(sleep(int(1, t.i64), pv), t.unit)),
                se(print(vec![f.bw(l)], t)),
            ])
        };
        let body = vec![phase(&f, x, "a", "b"), phase(&f, y, "c", "d")];
        pb.add_fn(f.build_async(body))
    };
    let mut f = FB::new("main", pv);
    let p = f.local("p", pv);
    let body = vec![
        se(print(vec![await_(call(seq, vec![], pi), t.i64)], t)),
        let_(p, call(phases, vec![], pv)),
        se(await_(f.mv(p), t.unit)),
    ];
    pb.add_main(f.build_async(body));
    pb.finish()
}

/// `async function main() { const p = f(); await p; }` — `f`'s boxed constructor (the promise
/// value) is what the cancellation tests call.
fn main_awaiting_value(pb: &mut PB, f: velt_sema::hir::DefId) {
    let pv = pb.promise(pb.t.unit);
    let mut m = FB::new("main", pv);
    let p = m.local("p", pv);
    let body = vec![let_(p, call(f, vec![], pv)), se(await_(m.mv(p), pb.t.unit))];
    pb.add_main(m.build_async(body));
}

#[test]
fn disjoint_suspension_locals_share_state_bytes() {
    let p = sequential_program();
    let v = lower_ok(&p);
    let child = state_size(&v, "delayed");
    // result + tag, `a`, and ONE child state: the two awaited children never coexist.
    let seq = state_size(&v, "seq");
    assert!(
        seq <= 16 + 8 + child,
        "seq state {seq} bytes, child {child}\n{v}"
    );
    // tag + one string + one sleep future: `x` and `y` live in disjoint blocks.
    assert!(state_size(&v, "phases") <= 8 + 24 + 8, "{v}");
    let out = run(&p);
    assert_eq!(out.stdout, "42\nab\ncd\n");
    for polls in [0, 1, 2, 3] {
        let r = poll_then_drop(&v, "_V6phases", &[], polls);
        assert_eq!(r.live_allocs, 0, "leak after {polls} polls");
    }
}

/// ```text
/// async function tagged(ms: i64, s: string): Promise<string> { await sleep(ms); return s + "!"; }
/// async function both() {
///   const p = tagged(3, "c");                    // a promise value (heap future)
///   const r = await Promise.all([tagged(1, "a"), p, tagged(5, "b")]);
///   console.log(r[0], r[1], r[2]);
/// }
/// ```
fn all_program() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let (ps, pv) = (pb.promise(t.str), pb.promise(t.unit));
    let (aps, sa) = (pb.arr(ps), pb.arr(t.str));
    let psa = pb.promise(sa);
    let tagged = {
        let mut f = FB::new("tagged", ps);
        let ms = f.param("ms", t.i64, PassMode::Copy);
        let s_ = f.param("s", t.str, PassMode::Owned);
        let body = vec![
            se(await_(sleep(f.cp(ms), pv), t.unit)),
            ret(Some(concat(f.bw(s_), s("!", t), t))),
        ];
        pb.add_fn(f.build_async(body))
    };
    let both = {
        let mut f = FB::new("both", pv);
        let p = f.local("p", ps);
        let r = f.local("r", sa);
        let tg = |ms, v: &str| call(tagged, vec![int(ms, t.i64), s(v, t)], ps);
        let at = |f: &FB, i| index(f.bw(r), int(i, t.usize), U::Borrow, t.str);
        let body = vec![
            let_(p, tg(3, "c")),
            let_(
                r,
                await_(
                    promise_all(array(vec![tg(1, "a"), f.mv(p), tg(5, "b")], aps), psa),
                    sa,
                ),
            ),
            se(print(vec![at(&f, 0), at(&f, 1), at(&f, 2)], t)),
        ];
        pb.add_fn(f.build_async(body))
    };
    main_awaiting_value(&mut pb, both);
    pb.finish()
}

#[test]
fn promise_all_of_literal_is_inline() {
    let p = all_program();
    let v = lower_ok(&p);
    assert!(
        !v.externs
            .iter()
            .any(|e| e.symbol == "velt_rt_all" || e.symbol == "velt_rt_all_with_drop"),
        "{v}"
    );
    let out = run(&p);
    assert_eq!(out.stdout, "a! c! b!\n");
    // Cancelled before start, with all pending, with `a` finished (its result is dropped), with
    // `a` and `c` finished; then run to completion.
    for polls in [0, 1, 2, 4] {
        let r = poll_then_drop(&v, "_V4both", &[], polls);
        assert!(!r.ready, "polls={polls}");
        assert_eq!(r.live_allocs, 0, "leak after {polls} polls");
    }
    let done = poll_then_drop(&v, "_V4both", &[], 20);
    assert!(done.ready);
    assert_eq!(done.stdout, "a! c! b!\n");
    assert_eq!(done.live_allocs, 0);
}

/// `async function viaRt() { const xs = [tagged(1, "a"), tagged(5, "b")];
/// console.log((await Promise.all(xs)).length); }` — an array value goes to the runtime.
#[test]
fn promise_all_value_drops_finished_results_on_cancel() {
    let mut pb = PB::new();
    let t = pb.t;
    let (ps, pv) = (pb.promise(t.str), pb.promise(t.unit));
    let (aps, sa) = (pb.arr(ps), pb.arr(t.str));
    let psa = pb.promise(sa);
    let tagged = {
        let mut f = FB::new("tagged", ps);
        let ms = f.param("ms", t.i64, PassMode::Copy);
        let s_ = f.param("s", t.str, PassMode::Owned);
        let body = vec![
            se(await_(sleep(f.cp(ms), pv), t.unit)),
            ret(Some(concat(f.bw(s_), s("!", t), t))),
        ];
        pb.add_fn(f.build_async(body))
    };
    let mut f = FB::new("viaRt", pv);
    let xs = f.local("xs", aps);
    let tg = |ms, v: &str| call(tagged, vec![int(ms, t.i64), s(v, t)], ps);
    let len = intr(
        I::ArrayLen,
        vec![await_(promise_all(f.mv(xs), psa), sa)],
        t.usize,
    );
    let body = vec![
        let_(xs, array(vec![tg(1, "a"), tg(5, "b")], aps)),
        se(print(vec![len], t)),
    ];
    let via_rt = pb.add_fn(f.build_async(body));
    main_awaiting_value(&mut pb, via_rt);
    let v = lower_ok(&pb.finish());
    assert!(
        v.externs
            .iter()
            .any(|e| e.symbol == "velt_rt_all_with_drop"),
        "{v}"
    );
    for polls in [1, 2, 3] {
        let r = poll_then_drop(&v, "_V5viaRt", &[], polls);
        assert!(!r.ready);
        assert_eq!(r.live_allocs, 0, "leak after {polls} polls");
    }
    let done = poll_then_drop(&v, "_V5viaRt", &[], 20);
    assert_eq!((done.ready, done.stdout.as_str()), (true, "2\n"));
    assert_eq!(done.live_allocs, 0);
}
