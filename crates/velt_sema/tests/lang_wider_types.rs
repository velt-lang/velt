//! Conversions TypeScript allows between types that differ only in a wider part: containers of
//! `any` and `unknown` (`Ctx::same_but_any_unknown`, #1185), function results
//! (`body/expr/fn_widen.rs`, #943), record spreads of another value type
//! (`body/expr/record_literal.rs`) and chained assignments of a literal (`body/expr/assign.rs`).

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn containers_of_any_and_unknown_are_one_type() {
    ok_src(
        "function main() {
           const r: Record<string, any> = { a: 1 };
           const u: Record<string, unknown> = r;
           const back: Record<string, any> = u;
           const xs: unknown[] = [1];
           const ys: any[] = xs;
           const m: Map<string, unknown> = new Map<string, any>();
           const t: [unknown, string] = [1, \"s\"] as [any, string];
           console.log(xs === ys, back === r, m.size, t[1]);
         }",
    );
}

#[test]
fn a_class_at_any_is_not_one_at_unknown() {
    // Its members may depend on the argument: only containers are the same value.
    let r = err_src(
        "class Box<T> { v: T; constructor(v: T) { this.v = v; } }
         function main() {
           const b: Box<any> = new Box<any>(1);
           const c: Box<unknown> = b;
           console.log(c);
         }",
    );
    assert!(r.contains("Box<unknown>"), "{r}");
}

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

#[test]
fn a_record_spread_converts_its_values() {
    ok_src(
        "function merge(a: Record<string, string>): Record<string, any> { return { ...a, x: 1 }; }
         function main() { console.log(merge({ k: \"v\" })); }",
    );
    let r = err_src(
        "function bad(a: Record<string, number>): Record<string, string> { return { ...a }; }
         function main() { console.log(bad({})); }",
    );
    assert!(r.contains("expected string, found f64"), "{r}");
}

#[test]
fn an_assignment_of_a_literal_as_a_value_without_an_expected_type_reads_the_target() {
    ok_src(
        "function main() {
           let s1: string | null = \"a\";
           const z = (s1 = null);
           let line: string | null;
           if ((line = null) === null) console.log(z, s1, line);
         }",
    );
}

#[test]
fn a_record_keyed_by_string_literals_spreads_into_one_keyed_by_string() {
    ok_src(
        "function main() {
           const keyed: Record<\"a\" | \"b\", string> = { a: \"1\", b: \"2\" };
           const wide: Record<string, any> = { ...keyed };
           const same: Record<string, string> = { ...keyed };
           console.log(wide, same);
         }",
    );
}
