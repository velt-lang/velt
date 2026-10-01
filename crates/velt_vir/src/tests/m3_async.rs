//! `tests/golden/m3/async_basic.vlt` hand-lowered to HIR: embedded awaits, `sleep`,
//! `Promise.all` over compiled calls and over leaf futures, `spawn` + join, awaits in loops.

use super::programs_m3::async_basic;
use super::{m3_golden, run};

#[test]
fn async_basic_golden() {
    let out = run(&async_basic());
    assert_eq!(out.stdout, m3_golden("async_basic"));
    assert_eq!(out.code, 0);
}
