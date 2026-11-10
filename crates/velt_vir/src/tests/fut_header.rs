//! Heap futures polled and dropped through their header (rt_abi_async.md §1, lower/raw_memory.rs):
//! std's `futPoll` and `futDrop`, and lowering's own polls and drops, are indirect calls of the
//! header's entries, with no runtime function in between. The interpreter's heap futures hold
//! its emulated runtime's entries there, so a call through the wrong entry, or of a runtime
//! function, fails here.

use super::{interp, lower_ok, std_src};
use crate::vir::{Callee, Terminator};

/// The number of indirect calls in `v`'s function `symbol`.
fn indirect_calls(v: &crate::vir::Program, symbol: &str) -> usize {
    let f = v
        .funcs
        .iter()
        .find(|f| f.symbol == symbol)
        .unwrap_or_else(|| panic!("no function `{symbol}` in:\n{v}"));
    f.blocks
        .iter()
        .filter(|b| {
            matches!(
                &b.term,
                Terminator::Call {
                    callee: Callee::Ptr { .. },
                    ..
                }
            )
        })
        .count()
}

#[test]
fn fut_poll_and_fut_drop_call_through_the_header() {
    let p = std_src::check_std_src(
        "fut_header_test",
        "import { futDrop, futPoll } from \"velt/lang/intrinsics\";
         declare function velt_rt_sleep(ms: i64): usize;
         function main() {
           const f = velt_rt_sleep(0);
           // SAFETY: `f` is a live heap future, polled once and dropped once.
           unsafe {
             console.log(futPoll(f, 8));
             futDrop(f);
           }
         }",
    );
    let v = lower_ok(&p);
    assert!(
        !v.externs
            .iter()
            .any(|e| e.symbol.starts_with("velt_rt_fut_")),
        "{v}"
    );
    assert_eq!(indirect_calls(&v, "_V4main"), 2, "{v}");
    let out = interp::run(&v);
    assert_eq!(out.stdout, "1\n");
    assert_eq!(out.live_allocs, 0, "the future was not dropped\n{v}");
}

#[test]
fn awaiting_a_promise_value_polls_and_drops_it_through_the_header() {
    let p = std_src::check_src(
        "async function main() {
           const p: Promise<void> = sleep(1);
           await p;
           console.log(\"woke\");
         }",
    );
    let v = lower_ok(&p);
    // The interpreter has no runtime function for either: only the header's entries.
    let out = interp::run(&v);
    assert_eq!(out.stdout, "woke\n");
    assert_eq!(out.live_allocs, 0, "{v}");
}
