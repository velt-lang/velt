//! What the runtime answers about a promise in a program that runs nothing on the executor in
//! Velt: nothing, on any target. Natively std answers (`velt_rt_fut_peek` is std's `schedPeek`
//! since #2196 9f); on WebAssembly, whose runtime has no promises, the answer is lowered inline
//! (lower/sched.rs `no_runtime_promises`, #3905), so the module links no such function.

use super::c_wasi::{check_for, lower_checked_for};
use crate::vir;

/// Promise-typed members held as `any` in a program with no async code: printed, copied and
/// compared, so their peek, share and identity glue is lowered.
const SRC: &str = include_str!("../../../../tests/golden/lang/any_promise_no_async.velt");

const ASKS: [&str; 3] = ["velt_rt_fut_peek", "velt_rt_fut_share", "velt_rt_fut_id"];

fn asks(target: &str) -> Vec<String> {
    let (p, sm) = check_for(SRC, target);
    let v: vir::Program = lower_checked_for(&p, &sm, target).unwrap_or_else(|e| panic!("{e}"));
    (v.externs.iter())
        .filter(|x| ASKS.contains(&x.symbol.as_str()))
        .map(|x| x.symbol.clone())
        .collect()
}

#[test]
fn webassembly_asks_the_runtime_nothing_about_promises() {
    for target in ["wasm32-wasip1", "wasm32-unknown-unknown"] {
        assert_eq!(asks(target), Vec::<String>::new(), "{target}");
    }
}

#[test]
fn a_native_target_asks_the_runtime_nothing_either() {
    // std answers there; without `no_runtime_promises` the WebAssembly test above fails.
    let native = asks("x86_64-unknown-linux-gnu");
    assert_eq!(native, Vec::<String>::new());
}
