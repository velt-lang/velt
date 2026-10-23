//! Conversions TypeScript allows between types that differ only in a wider part: function results
//! (`body/expr/fn_widen.rs`, #943) and chained assignments of a literal (`body/expr/assign.rs`).

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn a_function_converts_to_a_function_type_with_a_wider_result() {
    ok_src(
        "function run(f: () => any): void { console.log(f()); }
         function nothing(): void {}
         async function scope<T>(fn: () => T | Promise<T>): Promise<T> { return fn(); }
         function main() {
           run(nothing);
           const g: () => number | string = () => 1;
           console.log(g());
           scope(async (): Promise<number> => 2).then((v) => console.log(v));
         }",
    );
}

#[test]
fn a_result_that_does_not_convert_is_still_a_mismatch() {
    let r = err_src(
        "function main() {
           const f: () => number = () => 1;
           const g: () => string = f;
           console.log(g());
         }",
    );
    assert!(r.contains("mismatched types"), "{r}");
    assert!(r.contains("expected () => string"), "{r}");
}

#[test]
fn a_chained_assignment_of_null_gives_each_target_the_null() {
    ok_src(
        "function main() {
           let a: string[] | null = [];
           let b: number[] | null = [];
           a = b = null;
           console.log(a, b);
         }",
    );
}

