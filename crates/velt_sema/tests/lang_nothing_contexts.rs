//! `null` and `undefined` where the context gives no type, or a type that already holds an
//! error; casts and callbacks that TypeScript types through `undefined`; `Record` reads that
//! narrow like fields; variables that closures assign, read where their declared type is
//! expected (#930, #1481).

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn null_without_a_type_to_take_is_null_alone() {
    ok_src(
        "const none = null;
         const o = { value: null, count: 1 };
         const frozen = Object.freeze({ state: \"pending\" as const, value: null });
         function main() { console.log(none, o.value, o.count, frozen.value); }",
    );
}

#[test]
fn let_initialized_with_null_still_asks_for_a_type() {
    let r = err_src("function main() { let x = null; x = 5; console.log(x); }");
    assert!(r.contains("cannot infer the type of `null`"), "{r}");
    assert_eq!(r.matches("error").count(), 1, "{r}");
}

#[test]
fn null_bang_is_an_error_not_a_panic() {
    let r = err_src(
        "interface Ctx { app: { name: string }; n: number }
         function main() { const c: Ctx = { app: null!, n: 1 }; console.log(c.n); }",
    );
    assert!(r.contains("`null!` is never a value"), "{r}");
    assert!(
        r.contains("make the type `{ name: string }` hold `null`"),
        "{r}"
    );
}

#[test]
fn null_arm_next_to_a_broken_arm_reports_nothing_more() {
    let r = err_src(
        "function main() {
           const f = true ? missing(1) : null;
           const g = true ? (1 as unknown as { m: () => void }).m.bind(1) : null;
           console.log(f, g);
         }",
    );
    assert!(!r.contains("cannot infer the type of `null`"), "{r}");
}

#[test]
fn cast_of_a_nullable_record_to_another_record_type() {
    ok_src(
        "function count(props: Record<string, any>): number { return Object.keys(props).length; }
         function jsxs(props?: Record<string, unknown>): number {
           return count(props as Record<string, any>);
         }
         function main() { console.log(jsxs({ a: 1 })); }",
    );
}

#[test]
fn then_giving_undefined_where_a_void_promise_is_expected() {
    ok_src(
        "function refresh(ps: Promise<number>[]): Promise<void> {
           return Promise.all(ps).then(() => undefined);
         }
         async function main() { await refresh([Promise.resolve(1)]); }",
    );
}

#[test]
fn record_reads_with_a_fixed_key_narrow_like_fields() {
    ok_src(
        "function group(items: string[]): Record<string, string[]> {
           const named: Record<string, string[]> = {};
           for (const it of items) {
             const slot = it.slice(0, 1);
             if (!named[slot]) { named[slot] = []; }
             named[slot].push(it);
           }
           return named;
         }
         function add(r: Record<string, number[]>, k: string): void {
           if (!r[k]) r[k] = [];
           r[k].push(1);
         }
         function copies(named: Record<string, number[]>): number {
           let n = 0;
           for (const name in named) { const list = named[name]; n += list.length; }
           return n;
         }
         function main() { add({}, \"a\"); console.log(group([\"ab\"]), copies({ a: [1] })); }",
    );
}

#[test]
fn record_read_after_its_key_is_reassigned_is_not_narrowed() {
    let r = err_src(
        "function moved(r: Record<string, number[]>, k: string): void {
           if (!r[k]) r[k] = [];
           k = \"other\";
           r[k].push(1);
         }
         function main() { moved({}, \"a\"); }",
    );
    assert!(r.contains("no method named `push`"), "{r}");
}

#[test]
fn record_read_with_a_let_key_is_not_narrowed() {
    let r = err_src(
        "function f(r: Record<string, number[]>): void {
           let k = \"a\";
           if (!r[k]) r[k] = [];
           r[k].push(1);
         }
         function main() { f({}); }",
    );
    assert!(r.contains("no method named `push`"), "{r}");
}

#[test]
fn closure_assigned_variable_reads_as_declared_where_that_type_is_expected() {
    ok_src(
        "function show(v: number | null): string { return v === null ? \"none\" : \"some\"; }
         function main() {
           let n: number | null = 1;
           const f = () => { n = null; };
           f();
           console.log(n, show(n), `${n}`, String(n));
         }",
    );
}
