//! Moves dataflow: soft (string) moves that become copies when the place is used again.

mod common;

use common::programs::ok_src;

#[test]
fn string_moves_after_an_earlier_soft_move_become_copies() {
    ok_src(
        "function main() { const s = `a${1}`; const c = true;
           console.log((c ? s : \"b\").length, s);
           const none: string | null = null; console.log(`${none ?? s}`, s); }",
    );
}
