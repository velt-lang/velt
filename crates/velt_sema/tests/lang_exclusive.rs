//! Exclusive access: a place a call may modify (inferred) is reachable through no other argument.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};
use velt_sema::hir::PassMode;

const FNS: &str = "
function swap(a: i64[], b: i64[]): void { const t = a[0]; a[0] = b[0]; b[0] = t; }
function fill(a: i64[], b: i64[]): void { a.push(b.length as i64); }
function sum(a: i64[], b: i64[]): i64 { return a[0] + b[0]; }
function pushOne(a: i64[], x: i64): void { a.push(x); }
function popIt(xs: i64[]): i64 { return xs.pop() ?? 0; }
class Box { items: i64[] = []; other: i64[] = []; n: i64 = 0; }
";

fn ok_main(body: &str) {
    ok_src(&format!("{FNS} function main() {{ {body} }}"));
}

fn err_main(body: &str) -> String {
    err_src(&format!("{FNS} function main() {{ {body} }}"))
}

const MSG: &str = "cannot use `xs` here: this call may modify it through another argument";

#[test]
fn distinct_places_shared_borrows_and_copies_are_fine() {
    ok_main(
        "const xs: i64[] = [1]; const ys: i64[] = [2]; swap(xs, ys); fill(xs, ys);
         console.log(sum(xs, xs)); pushOne(xs, xs[0]); pushOne(xs, xs.length as i64);
         xs.push(xs.length as i64); fill(xs, [xs[0]]); fill(xs, ys.slice(0, 1));
         const b = new Box(); fill(b.items, b.other); pushOne(b.items, b.n);
         const grid: i64[][] = [[1], [2]]; fill(grid[0], ys);
         let total = 0; xs.forEach((x) => { total += x; }); console.log(total);",
    );
}

#[test]
fn same_place_twice_with_a_modified_param_is_an_error() {
    let r = err_main("const xs: i64[] = [1]; swap(xs, xs);");
    assert!(r.contains(MSG), "{r}");
    assert!(r.contains("may be modified through this argument"), "{r}");
    let r = err_main("const xs: i64[] = [1]; fill(xs, xs);");
    assert!(r.contains(MSG), "{r}");
}

#[test]
fn overlapping_places_conflict() {
    let r = err_main("const b = new Box(); fill(b.items, b.items);");
    assert!(r.contains("cannot use `b.items` here"), "{r}");
    let r = err_main("const grid: i64[][] = [[1], [2]]; fill(grid[0], grid[1]);");
    assert!(r.contains("cannot use `grid[..]` here"), "{r}");
    let r = err_main(
        "const grid: i64[][] = [[1], [2]]; for (const row of grid) { fill(grid[0], row); }",
    );
    assert!(
        r.contains(
            "cannot use `row` here: this call may modify `grid[..]` through another argument"
        ),
        "{r}"
    );
}

#[test]
fn closure_arguments_count_as_uses_of_their_captures() {
    let r = err_main("const xs: i64[] = [1]; xs.forEach((x) => { xs.push(x); });");
    assert!(r.contains(MSG), "{r}");
    assert!(r.contains("`xs` is modified by this closure"), "{r}");
    let r = err_src(
        "function both(f: () => void, g: () => i64): void { f(); console.log(g()); }
         function main() { const xs: i64[] = []; both(() => { xs.push(1); }, () => xs.length as i64); }",
    );
    assert!(r.contains(MSG), "{r}");
    // A Copy variable read by a closure is copied into it: no alias.
    ok_src(
        "function both(f: () => void, g: () => i64): void { f(); console.log(g()); }
         function main() { let n = 0; both(() => { n += 1; }, () => n); }",
    );
}

#[test]
fn mutex_with_callback_must_not_capture_the_mutex() {
    ok_main("const m = new Mutex<i64[]>([1]); m.with((v) => { v.push(1); });");
    let r = err_main(
        "const m = new Mutex<i64[]>([1]);
         m.with((v) => { v.push(m.with((w) => w.length) as i64); });",
    );
    assert!(
        r.contains("cannot use `m` here: this call may modify it through another argument"),
        "{r}"
    );
}

#[test]
fn modifying_methods_and_this() {
    let r = err_src(
        "class Acc { items: i64[] = []; total: i64 = 0;
           addAll(xs: i64[]): void { for (const x of xs) { this.items.push(x); } }
           bad(): void { this.addAll(this.items); } }
         function main() { new Acc().bad(); }",
    );
    assert!(
        r.contains(
            "cannot use `this.items` here: this call may modify `this` through another argument"
        ),
        "{r}"
    );
    let r = err_src(
        "class Acc { items: i64[] = []; total: i64 = 0;
           bad(): void { this.items.forEach((x) => { this.total += x; }); } }
         function main() { new Acc().bad(); }",
    );
    assert!(r.contains("`this` is modified by this closure"), "{r}");
}

#[test]
fn mutating_or_moving_while_borrowed_by_the_call() {
    let r = err_main("const xs: i64[] = [1]; console.log(sum(xs, [popIt(xs)]));");
    assert!(
        r.contains("cannot modify `xs` here: it is already borrowed by this call"),
        "{r}"
    );
    let r = err_main("const xs: i64[] = [1]; xs.push(popIt(xs));");
    assert!(r.contains(MSG), "{r}");
    let r = err_src(
        "function keep(a: string[], b: string[]): i64 { const out: string[][] = []; out.push(b);
           return a.length as i64; }
         function main() { const ss: string[] = [\"a\"]; console.log(keep(ss, ss)); }",
    );
    assert!(
        r.contains("cannot move `ss` here: it is already borrowed by this call"),
        "{r}"
    );
}

#[test]
fn modification_on_any_path_counts() {
    // Only one branch modifies `a`; only a helper called from a loop modifies `this`.
    let r = err_src(
        "function maybe(a: i64[], b: i64[], c: bool): void { if (c) { a.push(b.length as i64); } }
         function main() { const xs: i64[] = [1]; maybe(xs, xs, false); }",
    );
    assert!(r.contains(MSG), "{r}");
    let r = err_src(
        "class Acc { items: i64[] = [];
           add(x: i64): void { this.items.push(x); }
           addAll(xs: i64[]): void { for (const x of xs) { this.add(x); } } }
         function main() { const a = new Acc(); a.addAll(a.items); }",
    );
    assert!(
        r.contains("cannot use `a.items` here: this call may modify `a` through another argument"),
        "{r}"
    );
    // Reading only: fine.
    ok_src(
        "class Acc { items: i64[] = [];
           count(xs: i64[]): number { return this.items.length + xs.length; } }
         function main() { const a = new Acc(); console.log(a.count(a.items)); }",
    );
}

#[test]
fn callbacks_that_modify_their_argument() {
    let r = err_src(
        "function main() { const grid: i64[][] = [[1]];
           grid.forEach((row) => { row.push(grid.length as i64); }); }",
    );
    assert!(
        r.contains("cannot use `grid` here: this call may modify it through another argument"),
        "{r}"
    );
    // A callback that only reads its argument may see the receiver.
    ok_src(
        "function main() { const grid: i64[][] = [[1]];
           grid.forEach((row) => { console.log(row.length + grid.length); }); }",
    );
}

#[test]
fn closure_params_may_alias_each_other() {
    let r = err_src(
        "function append(to: string[], from: string[]): void { for (const s of from) { to.push(s.clone()); } }
         function main() { const f = (a: string[], b: string[]) => { append(a, b); };
           const xs: string[] = []; f(xs, xs); }",
    );
    assert!(r.contains("cannot use `b` here"), "{r}");
}

#[test]
fn function_values_may_write_what_they_receive() {
    // `apply` hands `xs` to an unknown function: never read-only, but not a modified param for
    // its callers (the call supplying the function value accounts for it).
    let p = ok_src(
        "function apply(xs: i64[], f: (v: i64[]) => void): void { f(xs); }
         function main() { const m = new Map<string, i64[]>(); m.set(\"a\", []);
           m.forEach((v, k) => { console.log(k, m.size); });
           const ys: i64[] = []; apply(ys, (v) => { console.log(v.length); }); }",
    );
    let apply = func(&p, "apply");
    assert_eq!(apply.params[0].mode, PassMode::Borrow);
    assert!(apply.body.locals[0].mutable);
    let r = err_src(
        "function apply(xs: i64[], f: (v: i64[]) => void): void { f(xs); }
         function main() { const ys: i64[] = []; apply(ys, (v) => { v.push(ys.length as i64); }); }",
    );
    assert!(
        r.contains("cannot use `ys` here: this call may modify it through another argument"),
        "{r}"
    );
}
