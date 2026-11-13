//! `tests/golden/m3/async_basic.velt`: embedded awaits, `sleep`, `Promise.all` over compiled
//! calls and over leaf futures, `spawn` + join, awaits in loops. It prints numbers through std's
//! hooks (#2197), so it is loaded from source with the real std (std_src.rs).

use super::{m3_golden, run_src};

#[test]
#[ignore = "#2921"]
fn async_basic_golden() {
    let out = run_src(&super::golden_src("m3", "async_basic"));
    assert_eq!(out.stdout, m3_golden("async_basic"));
    assert_eq!(out.code, 0);
}
