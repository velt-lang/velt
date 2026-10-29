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
fn closure_params_of_unrelated_classes_are_separate() {
    // A value is never an instance of two classes neither of which extends the other: such
    // params of a closure (a request and a response) are separate places.
    let classes = "class In { data: string[] = []; } class Out { data: string[] = []; }
         function fill(o: Out, i: In): void { for (const s of i.data) { o.data.push(s.clone()); } }";
    ok_src(&format!(
        "{classes} function main() {{ const f = (i: In, o: Out) => {{ fill(o, i); }}; f(new In(), new Out()); }}"
    ));
    // Instances of one class may be the same object.
    let r = err_src(&format!(
        "{classes} function main() {{ const f = (a: Out, b: Out) => {{ fill(a, new In()); a.data.push(b.data[0].clone()); twice(a, b); }}; f(new Out(), new Out()); }}
         function twice(a: Out, b: Out): void {{ a.data.push(\"x\"); console.log(b.data.length); }}"
    ));
    assert!(r.contains("cannot use `b` here"), "{r}");
    // So may an instance of a subclass and one of its base.
    let r = err_src(&format!(
        "{classes} class Sub extends Out {{}}
         function main() {{ const f = (o: Out, s: Sub, i: In) => {{ fill(o, i); grow(o, s); }}; f(new Out(), new Sub(), new In()); }}
         function grow(o: Out, s: Sub): void {{ o.data.push(\"x\"); console.log(s.data.length); }}"
    ));
    assert!(r.contains("cannot use `s` here"), "{r}");
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

#[test]
fn an_argument_may_change_the_inside_of_an_object_another_one_passes() {
    // `get` modifies `this.src` while the argument is evaluated, before `set` runs: JavaScript's
    // `set` sees the changed object too.
    ok_src(
        "class Src { n: i64 = 0; get(i: i64): i64 { this.n++; return i + this.n; } }
         class User {
           src: Src = new Src(); v: i64 = 0;
           set(x: i64) { this.v = x; }
           step() { this.set(this.src.get(1)); }
         }
         function main() { const u = new User(); u.step(); console.log(u.v); }",
    );
    // Replacing what is passed is still a conflict: the callee would see the new array.
    let r =
        err_main("const b = new Box(); fill(b.items, [popIt(b.items), b.items.length as i64]);");
    assert!(r.contains("b.items"), "{r}");
}

#[test]
fn an_argument_may_not_change_an_inline_object_another_one_passes() {
    // `o.pt` is held inline and passed as a copy made before `o.pt.x = 5` runs: Node's `f`
    // would see the change, Velt's would not, so it is still a conflict.
    let r = err_src(
        "type Pt = { x: number; y: number };
         class O { pt: Pt = { x: 1, y: 2 }; }
         function f(p: Pt, n: number): number { return p.x * 10 + n; }
         function main() { const o = new O(); console.log(f(o.pt, (o.pt.x = 5))); }",
    );
    assert!(r.contains("o.pt"), "{r}");
}

#[test]
fn an_argument_may_change_the_inside_of_an_array_another_one_passes() {
    // `grow` pushes onto `g[1]` while the argument is evaluated: `g` is passed by reference, so
    // `h` sees the longer row, as Node's does.
    ok_src(
        "function grow(a: i64[]): i64 { a.push(7); return 1; }
         function h(g: i64[][], n: i64): i64 { return g[1].length as i64 + n; }
         function main() { const g: i64[][] = [[1], [2]]; console.log(h(g, grow(g[1]))); }",
    );
    // A change through the passed value itself, or replacing it, is still a conflict.
    let r = err_src(
        "class B { items: i64[] = [1]; }
         function setFirst(b: B): i64 { b.items[0] = 8; return 0; }
         function show(b: B, n: i64): i64 { return b.items[0] + n; }
         function main() { const b = new B(); console.log(show(b, setFirst(b))); }",
    );
    assert!(r.contains("`b`"), "{r}");
    let r = err_main("const b = new Box(); fill(b.items, [(b.items = []).length as i64]);");
    assert!(r.contains("b.items"), "{r}");
}

#[test]
fn an_argument_may_shorten_a_field_array_another_one_passes() {
    // `h.vs.length = 0` writes through a share of `h.vs`: it changes the array `h.vs` holds,
    // which `g` then sees, as Node's does.
    let h = "class V { s: string = \"y\"; }
             class H { vs: V[] = [new V(), new V()]; }
             function g(vs: V[], n: number): number { return vs.length + n; }
             function k(v: V, n: number): number { return v.s.length + n; }";
    ok_src(&format!(
        "{h} function main() {{ const h = new H(); console.log(g(h.vs, (h.vs.length = 0))); }}"
    ));
    // An element passed while the array is shortened would be freed under the callee.
    let r = err_src(&format!(
        "{h} function main() {{ const h = new H(); console.log(k(h.vs[0], (h.vs.length = 0))); }}"
    ));
    assert!(r.contains("cannot modify `h.vs` here"), "{r}");
}

#[test]
fn an_argument_may_not_replace_a_field_another_one_passes_through_a_narrowed_field() {
    // `h.b.vs = mk()` on a narrowed `h.b` is lowered as a write to `vs` through a share of
    // `h.b`: it replaces the array (or instance) passed, so it stays an error.
    let src = "class V { n: number = 1; }
               class C { vs: V[] = [new V()]; }
               class B { vs: V[] = [new V(), new V()]; c: C = new C(); }
               class H { b: B | undefined = new B(); }
               function mk(): V[] { return [new V()]; }
               function mkc(): C { return new C(); }
               function g(vs: V[], n: number): number { vs.push(new V()); return vs.length + n; }
               function gc(c: C, n: number): number { c.vs.push(new V()); return c.vs.length + n; }";
    for (call, place) in [
        ("g(h.b.vs, (h.b.vs = mk()).length)", "h.b.vs"),
        ("gc(h.b.c, (h.b.c = mkc()).vs.length)", "h.b.c"),
    ] {
        let r = err_src(&format!(
            "{src} function main() {{ const h = new H(); if (h.b) {{ console.log({call}); }} }}"
        ));
        assert!(r.contains(&format!("`{place}`")), "{call}: {r}");
    }
}

#[test]
fn a_system_function_buffer_is_not_shortened_by_another_argument() {
    // A system function's buffer is a view of bytes: a later argument shortening the array
    // would free bytes it covers, read or written.
    let decl = "declare system function pread(fd: i32, buf: u8[], offset: i64): isize;
                declare system function write(fd: i32, buf: readonly u8[], n: usize): isize;
                class Holder { b: u8[] = Buffer.alloc(16); }";
    for call in [
        "pread(0, h.b, (h.b.length = 0) as i64)",
        "write(1, h.b, (h.b.length = 0) as usize)",
        "write(1, h.b.subarray(0, 8), (h.b.length = 0) as usize)",
    ] {
        let r = err_src(&format!(
            "{decl} function main() {{ const h = new Holder(); console.log({call}); }}"
        ));
        assert!(r.contains("`h.b` here"), "{call}: {r}");
    }
}

/// A renderer whose nested functions are closures that call each other: `mount` passes its
/// params on to `mountComponent` (`call`), which modifies the vnode.
fn renderer(call: &str) -> String {
    format!(
        "type V = {{ type: any; dom: any; kids: V[] }};
         type S = (v: V) => V;
         function touch(v: V): any {{ v.kids.push({{ type: \"t\", dom: null, kids: [] }}); return null; }}
         function make<E = any>(insert: (v: V, c: E) => void) {{
           function mount(vnode: V, container: E, before: any = null): void {{
             if (typeof vnode.type === \"object\") {{ {call}; return; }}
             insert(vnode, container);
             for (const k of vnode.kids) mount(k, container, null);
           }}
           function mountComponent(vnode: V, container: E, before: any, setup: S): void {{
             vnode.dom = before;
             mount(setup(vnode), container, null);
           }}
           return (v: V, c: E): void => {{ mount(v, c); }};
         }}
         function main() {{
           const r = make<V>((v, c) => {{ c.kids.push(v); }});
           const v: V = {{ type: {{ __setup: (x: V): V => ({{ type: \"p\", dom: null, kids: [] }}) }}, dom: null, kids: [] }};
           r(v, v);
         }}"
    )
}

#[test]
fn closures_pass_params_that_may_be_one_object_on_to_closures() {
    // `mountComponent` was checked with its params as one object (#1383).
    ok_src(&renderer(
        "mountComponent(vnode, container, before, vnode.type.__setup as S)",
    ));
}

#[test]
fn closures_passing_params_on_keep_other_conflicts() {
    // A part of one param with another: `mountComponent` was checked with a vnode and a
    // container that can't hold it.
    let r = err_src(&renderer(
        "mountComponent(vnode.kids[0], container, before, vnode.type.__setup as S)",
    ));
    assert!(
        r.contains("cannot use `vnode.kids[..]` here: this call may modify `container`"),
        "{r}"
    );
    // A param changed while another argument is evaluated.
    let r = err_src(&renderer(
        "mountComponent({ type: \"y\", dom: null, kids: [] }, container, touch(vnode), vnode.type.__setup as S)",
    ));
    assert!(
        r.contains("cannot use `vnode` here: this call may modify `container`"),
        "{r}"
    );
}
