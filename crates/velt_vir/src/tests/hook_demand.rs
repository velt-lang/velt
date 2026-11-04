//! Lowering calls a hook whose table entry lists constructs only where sema demanded it
//! (velt_common `hook_demand.rs`, `hir::Program::hooks_demanded`), checked in release compilers
//! too: a construct missing from the table is an ICE naming it, not a call of an unchecked stub.

use super::{lower_ok, lower_src, std_src};

const PRINTS_AN_INTEGER: &str =
    "function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n";

#[test]
fn a_demanded_hook_lowers() {
    let v = lower_src(PRINTS_AN_INTEGER);
    assert!(
        v.funcs.iter().any(|f| f.symbol.contains("intPush")),
        "no call of intPush:\n{v}"
    );
}

#[test]
#[should_panic(
    expected = "ICE: hook int_push reached by lowering but not demanded by sema (construct "
)]
fn a_hook_sema_did_not_demand_is_an_ice() {
    let mut p = std_src::check_src(PRINTS_AN_INTEGER);
    p.hooks_demanded = Default::default();
    lower_ok(&p);
}
