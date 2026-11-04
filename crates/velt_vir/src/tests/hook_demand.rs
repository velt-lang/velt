//! Lowering calls a hook whose table entry lists constructs only where sema demanded it
//! (velt_common `hook_demand.rs`, `hir::Program::hooks_demanded`), checked in release compilers
//! too: a construct missing from the table is an ICE naming it, not a call of an unchecked stub.

use velt_sema::hir;

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

/// Sema gives every index it builds an unsigned type, so no program reaches the out-of-bounds
/// message of a signed index (`Work::Oob(true)`, `int_text` for `IndexCheck`); lowering still
/// handles one, so this index is made signed by hand.
#[test]
fn a_signed_index_is_in_the_table() {
    let mut p = std_src::check_src(
        "function f(xs: i64[], i: i64): i64 {\n  return xs[i];\n}\n\
         function main() {\n  console.log(f([1], 0));\n}\n",
    );
    let hir::Def::Fn(f) = (p.defs.iter_mut())
        .find(|d| matches!(d, hir::Def::Fn(f) if f.name == "f"))
        .expect("f")
    else {
        unreachable!()
    };
    let signed = f.body.locals[1].ty;
    let Some(hir::StmtKind::Return(Some(e))) = f.body.block.stmts.last_mut().map(|s| &mut s.kind)
    else {
        panic!("f returns");
    };
    let hir::ExprKind::Index { index, .. } = &mut e.kind else {
        panic!("f returns an index: {e:?}");
    };
    if let hir::ExprKind::Cast(from) = &index.kind {
        *index = from.clone();
    }
    assert_eq!(index.ty, signed);
    let v = lower_ok(&p);
    assert!(
        v.funcs.iter().any(|f| f.symbol == "_Goob_i64"),
        "no signed out-of-bounds helper:\n{v}"
    );
}
