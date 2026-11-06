//! `tests/golden/m2/generics.velt`: generic structs/functions/classes (monomorphized per type
//! args), partial moves out of an owned param, interfaces with default methods used as generic
//! bounds and as values. It prints numbers through std's hooks (#2197), so it is loaded from
//! source with the real std (std_src.rs).

use super::{m2_golden, run_src};

#[test]
fn golden_generics() {
    let out = run_src(&super::golden_src("m2", "generics"));
    assert_eq!(out.stdout, m2_golden("generics"));
}
