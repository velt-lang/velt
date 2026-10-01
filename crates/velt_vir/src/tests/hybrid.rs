//! Hybrid promises (docs/reference/async.md): a directly awaited call is embedded in the caller's
//! state (no allocation, nothing started), a stored call is boxed and started at once
//! (`velt_rt_fut_start`), and `Promise.race` hands its promises to `velt_rt_race`.

use velt_sema::hir::{BinOp, Intrinsic as I, PassMode, Program};

use super::builder::*;
use super::builder_m2::*;
use super::builder_m3::*;
use super::{lower_ok, run};
use crate::vir;

/// Does any function call the runtime symbol `sym`?
fn calls(v: &vir::Program, sym: &str) -> bool {
    v.externs.iter().any(|e| e.symbol == sym)
}

/// ```text
/// async function step(x: i64): Promise<i64> { await sleep(1); return x + 1; }
/// async function main() {
///   // `how`: 0 = `console.log(await step(1))`, 1 = `const p = step(1); console.log(await p)`,
///   // 2 = `console.log(await Promise.race([step(1), step(10)]))`
/// }
/// ```
fn program(how: u8) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let (pi, pv) = (pb.promise(t.i64), pb.promise(t.unit));
    let api = pb.arr(pi);
    let step = {
        let mut f = FB::new("step", pi);
        let x = f.param("x", t.i64, PassMode::Copy);
        let body = vec![
            se(await_(sleep(int(1, t.i64), pv), t.unit)),
            ret(Some(bin(BinOp::Add, f.cp(x), int(1, t.i64)))),
        ];
        pb.add_fn(f.build_async(body))
    };
    let mut f = FB::new("main", pv);
    let p = f.local("p", pi);
    let st = |x| call(step, vec![int(x, t.i64)], pi);
    let body = match how {
        0 => vec![se(print(vec![await_(st(1), t.i64)], t))],
        1 => vec![let_(p, st(1)), se(print(vec![await_(f.mv(p), t.i64)], t))],
        _ => {
            let race = intr(I::PromiseRace, vec![array(vec![st(1), st(10)], api)], pi);
            vec![se(print(vec![await_(race, t.i64)], t))]
        }
    };
    pb.add_main(f.build_async(body));
    pb.finish()
}

#[test]
fn a_direct_await_allocates_and_starts_nothing() {
    let v = lower_ok(&program(0));
    assert!(!calls(&v, "velt_rt_fut_box"), "{v}");
    assert!(!calls(&v, "velt_rt_fut_start"), "{v}");
    assert_eq!(run(&program(0)).stdout, "2\n");
}

#[test]
fn a_stored_promise_is_boxed_and_started() {
    let v = lower_ok(&program(1));
    assert!(calls(&v, "velt_rt_fut_box"), "{v}");
    assert!(calls(&v, "velt_rt_fut_start"), "{v}");
    assert_eq!(run(&program(1)).stdout, "2\n");
}

#[test]
fn race_takes_the_first_result() {
    let v = lower_ok(&program(2));
    assert!(calls(&v, "velt_rt_race"), "{v}");
    assert_eq!(run(&program(2)).stdout, "2\n");
}
