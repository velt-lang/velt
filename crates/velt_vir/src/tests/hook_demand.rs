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

/// Sema's demand must cover what lowering prints an uncaught throw with (#2312's review: without
/// it, an ICE on these one-line programs that no golden shows).
#[test]
fn an_integer_thrown_and_not_caught_lowers() {
    lower_src("function main() { throw 5 as i32; }\n");
    lower_src("async function main() { throw 5 as i32; }\n");
}

/// The signature alone makes lowering print the error, whatever the body throws (#2312's review).
#[test]
fn a_declared_integer_error_nothing_throws_lowers() {
    lower_src("function f(): void throws i32 {}\nfunction main() { f(); }\n");
    lower_src("function f(): void throws u8 {}\nfunction main() { f(); }\n");
    lower_src("function main(): void throws i32 {}\n");
}
