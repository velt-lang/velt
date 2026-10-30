//! A `shared<T>` without a `Mutex` read like the `T` it holds (#36): index, `length`, `T`'s
//! methods and arguments where a `T` is expected go through `Intrinsic::SharedRead`; writes
//! through it are errors.

mod common;

use common::hir_walk::{calls, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Intrinsic};

fn shared_reads(src: &str, f: &str) -> usize {
    let p = ok_src(src);
    calls(func(&p, f))
        .into_iter()
        .filter(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::SharedRead)))
        .count()
}

#[test]
fn index_length_and_arguments_read_the_value_inside() {
    let n = shared_reads(
        "function sum(xs: u8[]): i64 { let t: i64 = 0; for (const x of xs) { t += x as i64; } return t; }
         function main() {
           const s = shared([1 as u8, 2, 3]);
           console.log(s[0], s.length, sum(s));
         }",
        "main",
    );
    assert_eq!(n, 3);
}

#[test]
fn methods_of_the_value_inside() {
    let n = shared_reads(
        "function main() {
           const s = shared(\"hello\");
           console.log(s.startsWith(\"he\"), s.indexOf(\"l\"), s.length, s[1]);
         }",
        "main",
    );
    assert_eq!(n, 4);
}

#[test]
fn clone_and_atomics_stay_on_the_shared_value() {
    let n = shared_reads(
        "function main() {
           const s = shared([1, 2]);
           const t = s.clone();
           const c = shared(0 as i64);
           c.add(1);
           console.log(c.get(), t.length);
         }",
        "main",
    );
    assert_eq!(n, 1);
    // A `shared<int>` changes: it is never read as a plain value.
    let r = err_src(
        "function f(n: i64): i64 { return n; } function main() { const c = shared(0 as i64); f(c); }",
    );
    assert!(
        r.contains("mismatched types") || r.contains("expected"),
        "{r}"
    );
}

#[test]
fn writes_through_it_are_errors() {
    let r = err_src("function main() { const s = shared([1, 2]); s[0] = 5; }");
    assert!(r.contains("a shared value is read-only"), "{r}");
    let r = err_src("function main() { const s = shared([1, 2]); s.push(3); }");
    assert!(r.contains("a shared value is read-only"), "{r}");
    let r = err_src(
        "function bump(xs: number[]) { xs[0] += 1; } function main() { const s = shared([1, 2]); bump(s); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // A function value may modify its argument.
    let r = err_src(
        "function main() { const f = (xs: number[]) => xs.length; const s = shared([1, 2]); console.log(f(s)); }",
    );
    assert!(r.contains("to a function value"), "{r}");
}

#[test]
fn a_mutex_inside_keeps_the_shared_value() {
    let r = err_src(
        "function main() { const s = shared(new Mutex<number[]>([1])); console.log(s.length); }",
    );
    assert!(r.contains("no field `length`"), "{r}");
}

#[test]
fn what_the_compiler_cannot_see_into_keeps_the_shared_value() {
    // A type parameter may be instantiated with a type that holds a `Mutex`.
    let r = err_src(
        "function first<T>(s: shared<T[]>): T { return s[0]; } function main() { console.log(first(shared([1]))); }",
    );
    assert!(r.contains("cannot index"), "{r}");
    // A class with subclasses: an object may have fields the base class doesn't declare.
    let r = err_src(
        "class A { n: number = 1; } class B extends A { m = new Mutex<number>(0); }
         function count(xs: A[]): number { return xs.length; }
         function main() { const s = shared([new A()]); console.log(count(s)); }",
    );
    assert!(r.contains("mismatched types"), "{r}");
    assert!(r.contains("cannot be read like a `A[]`"), "{r}");
}

#[test]
fn a_class_with_its_own_clone_keeps_the_shared_value() {
    // Copying what a read gets would run `clone()` on objects other threads read.
    let r = err_src(
        "class C { hits: number = 0; clone(): C { this.hits++; return new C(); } }
         function count(xs: C[]): number { return xs.length; }
         function main() { const s = shared([new C()]); console.log(count(s)); }",
    );
    assert!(r.contains("mismatched types"), "{r}");
}

#[test]
fn fields_and_for_of_read_the_value_inside() {
    let n = shared_reads(
        "struct Cfg { name: string; ports: number[]; }
         function main() {
           const s = shared<Cfg>({ name: \"x\", ports: [1, 2] });
           let t = 0;
           for (const p of s.ports) { t += p; }
           for (const c of shared(\"ab\")) { console.log(c); }
           console.log(s.name, t);
         }",
        "main",
    );
    assert_eq!(n, 3);
    let r = err_src(
        "struct Cfg { ports: number[]; } function main() { const s = shared<Cfg>({ ports: [1] }); s.ports.push(2); }",
    );
    assert!(r.contains("a shared value is read-only"), "{r}");
}

#[test]
fn typed_array_writes_through_it_are_errors() {
    for src in [
        "function main() { const s = shared(new Float64Array(3)); s[0] = 5; }",
        "function main() { const s = shared(new Float64Array(3)); s.set([1, 2], 0); }",
        "function main() { const s = shared(new Float64Array(3)); s.fill(1); }",
    ] {
        let r = err_src(src);
        assert!(r.contains("read-only"), "{src}: {r}");
    }
}

#[test]
fn a_method_returning_the_this_it_modifies_is_an_error() {
    // `bump` returns `this`, so it takes it owned: the change hides behind the move.
    let r = err_src(
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } }
         function twice(c: C): C { return c.bump().bump(); }
         function main() { const s = shared(new C()); s.bump(); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // So is one that calls such a method on `this`.
    let r = err_src(
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } again(): C { return this.bump(); } }
         function main() { const s = shared(new C()); s.again(); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // So is a function that changes it though it takes it owned: it would change a copy.
    let r = err_src(
        "function g(a: Float64Array): Float64Array { return a.fill(1); }
         function main() { const s = shared(new Float64Array(3)); g(s); }",
    );
    assert!(r.contains("read out of a `shared` value"), "{r}");
    // Reading through such a method's sibling that leaves `this` alone is fine.
    ok_src(
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } get(): number { return this.n; } }
         function main() { const s = shared(new C()); console.log(s.get()); }",
    );
}

#[test]
fn for_of_and_index_read_maps_sets_and_typed_arrays_inside() {
    let n = shared_reads(
        "function main() {
           const sm = shared(new Map<string, number>());
           const ss = shared(new Set<string>());
           const sf = shared(new Float64Array(2));
           let t = 0;
           for (const [k, v] of sm) { t += v; }
           for (const k of ss) { t += k.length; }
           for (const x of sf) { t += x; }
           console.log(t, sf[0]);
         }",
        "main",
    );
    // Each loop reads the value at least once (a typed array per length and element).
    assert!(n >= 4, "{n}");
}

#[test]
fn changing_loop_elements_of_it_is_an_error() {
    let class =
        "class C { n: number = 0; bump(): C { this.n += 1; return this; } inc() { this.n += 1; } }";
    for body in [
        "const s = shared(new Map<string, Uint8Array>()); for (const [k, v] of s) { v.fill(1); }",
        "const s = shared(new Set<C>()); for (const c of s) { c.bump(); }",
        "const s = shared(new Map<string, C>()); for (const [k, v] of s) { v.bump(); }",
        "const s = shared(new Map<string, C>()); for (const v of s.values()) { v.inc(); }",
        "const s = shared([new C()]); for (const c of s) { c.inc(); }",
        "const s = shared([[1], [2]]); for (const xs of s) { xs.push(3); }",
        "const s = shared([new C()]); for (const c of s.slice()) { c.n = 2; }",
    ] {
        let r = err_src(&format!("{class} function main() {{ {body} }}"));
        assert!(r.contains("the loop changes"), "{body}: {r}");
    }
    // Reading them is fine.
    ok_src(&format!(
        "{class} function main() {{ const s = shared(new Map<string, C>()); let t = 0; \
         for (const [k, v] of s) {{ t += v.n; }} for (const c of s.values()) {{ t += c.n; }} console.log(t); }}"
    ));
}

#[test]
fn changing_a_variable_bound_to_a_part_is_an_error() {
    let class = "class Cfg { ports: number[] = [1, 2]; c: C = new C(); } class C { n: number = 0; bump(): C { this.n += 1; return this; } }";
    for body in [
        "const s = shared(new Cfg()); const ps = s.ports; ps.push(3);",
        "const s = shared(new Cfg()); let { ports: ys } = s; ys.push(1);",
        "const s = shared(new Cfg()); const c = s.c; c.bump();",
        "const s = shared([[1], [2]]); const xs = s[0]; xs[0] = 5;",
    ] {
        let r = err_src(&format!(
            "{class} let G = shared(new Cfg()); function main() {{ {body} }}"
        ));
        assert!(r.contains("this changes"), "{body}: {r}");
    }
    let r = err_src(&format!(
        "{class} let G = shared(new Cfg()); function main() {{ let {{ ports: ys }} = G; ys.push(1); }}"
    ));
    assert!(r.contains("this changes"), "{r}");
    // Reading it, or changing a copy, is fine.
    ok_src(&format!(
        "{class} function main() {{ const s = shared(new Cfg()); const ps = s.ports; const [a, b] = s.ports; \
         const mine = s.ports.slice(); mine.push(3); console.log(ps.length, a + b, mine.length); }}"
    ));
}

#[test]
fn views_of_a_shared_typed_array_are_errors() {
    for src in [
        "function main() { const u = shared(new Uint8Array(3)); u.subarray(0, 2).fill(3); }",
        "function main() { const u = shared(new Uint8Array(3)); const w = u.subarray(1); w[0] = 9; }",
        "function main() { const u = shared(new Uint8Array(3)); const v = new Uint8Array(u.buffer); }",
    ] {
        let r = err_src(src);
        assert!(r.contains("would share its buffer"), "{src}: {r}");
    }
    ok_src("function main() { const u = shared(new Uint8Array(3)); const c = u.slice(0, 2); c[0] = 1; console.log(c[0], u[0]); }");
}

#[test]
fn rwlock_read_callbacks_cannot_modify_the_value() {
    ok_src(
        "function main() { const rw = shared(new RwLock<number[]>([1])); rw.write((v) => { v.push(2); }); console.log(rw.read((v) => v.length)); }",
    );
    let r = err_src(
        "function main() { const rw = shared(new RwLock<number[]>([1])); rw.read((v) => { v.push(2); }); }",
    );
    assert!(
        r.contains("the function passed to `read` modifies the locked value"),
        "{r}"
    );
    let r = err_src(
        "function bump(xs: number[]) { xs[0] += 1; } function main() { const rw = shared(new RwLock<number[]>([1])); rw.read((v) => bump(v)); }",
    );
    assert!(r.contains("modifies the locked value"), "{r}");
    // A named function is checked like a literal: an object nothing aliases has no count, so
    // read mode could not stop the write when it runs.
    let r = err_src(
        "class C { n: i64 = 0; } function bump(c: C) { c.n += 1; } function main() { const rw = shared(new RwLock<C>(new C())); rw.read(bump); }",
    );
    assert!(r.contains("modifies the locked value"), "{r}");
    ok_src(
        "class C { n: i64 = 0; } function get(c: C): i64 { return c.n; } function main() { const rw = shared(new RwLock<C>(new C())); console.log(rw.read(get)); }",
    );
    let r = err_src(
        "class C { n: i64 = 0; } function f(rw: shared<RwLock<C>>, cb: (c: C) => void) { rw.read(cb); } function main() { f(shared(new RwLock<C>(new C())), (c) => { c.n += 1; }); }",
    );
    assert!(
        r.contains("`read` needs a function literal or a named function"),
        "{r}"
    );
    let r = err_src("function main() { const rw = shared(new RwLock<number[]>([1])); rw.with((v) => v.length); }");
    assert!(r.contains("no method named `with`"), "{r}");
    let r = err_src(
        "function main() { const m = shared(new Mutex<number[]>([1])); m.read((v) => v.length); }",
    );
    assert!(r.contains("no method named `read`"), "{r}");
    let r = err_src("function main() { const rw = shared(new RwLock<number[]>([1])); rw.read(async (v) => v.length); }");
    assert!(
        r.contains("the function passed to `read` cannot be async"),
        "{r}"
    );
}

#[test]
fn a_function_changing_map_entries_it_loops_over_is_an_error() {
    // A map loop's bindings are not bound into the map, so this is the loop's own evidence
    // (`Ctx::loop_roots`) and not the parameter's mode.
    let defs = "class Item { v: number = 1; tags: string[] = []; }
        class Store { m: Map<string, Item> = new Map<string, Item>();
          walk(): number { let t = 0; for (const v of this.m.values()) { bump(v); t += v.v; } return t; } }
        function bump(i: Item) { i.v += 1000; }
        function walk(st: Store): number { let t = 0; for (const [k, v] of st.m) { bump(v); t += v.v; } return t; }
        function tag(st: Store) { for (const [k, v] of st.m) { v.tags.push(\"x\"); } }
        function sum(st: Store): number { let t = 0; for (const [k, v] of st.m) { t += v.v; } return t; }";
    for call in ["walk(s)", "s.walk()", "tag(s)"] {
        let r = err_src(&format!(
            "{defs} function main() {{ const s = shared(new Store()); console.log({call}); }}"
        ));
        assert!(r.contains("read out of a `shared` value"), "{call}: {r}");
    }
    ok_src(&format!(
        "{defs} function main() {{ const s = shared(new Store()); console.log(sum(s)); }}"
    ));
}

#[test]
fn json_of_a_shared_mutex_is_an_error() {
    // Its value is read only under the lock, which writing it as JSON doesn't take.
    let r = err_src(
        "function main() { const m = shared(new Mutex<number[]>([1])); console.log(JSON.stringify(m)); }",
    );
    assert!(r.contains("has no JSON form"), "{r}");
}

#[test]
fn changes_through_loops_aliases_closures_and_owned_params_are_errors() {
    // Each would change a copy of a part of the shared value, where JavaScript changes the
    // value itself.
    let defs = "class Item { v: number = 3; tags: string[] = []; }
        class Holder { st: Store = new Store(); }
        class Store { m: Map<string, Item> = new Map<string, Item>();
          mm: Map<string, Map<string, Item>> = new Map<string, Map<string, Item>>(); xs: Item[] = [new Item()]; }
        function bump(i: Item) { i.v += 1000; }
        let KEEP: Item[] = [];
        let KEEPS: Store[] = [];
        function keep(i: Item) { i.v += 1000; KEEP.push(i); }
        function adopt(st: Store, h: Holder) { st.m.get(\"k1\")!.v = 77; h.st = st; }
        function f(st: Store) { st.xs[0].v = 500; KEEPS.push(st); }
        async function walk(st: Store): Promise<void> { await Promise.resolve(0); for (const [k, v] of st.m) bump(v); }
        function* gen(st: Store): Generator<number> { for (const [k, v] of st.m) { bump(v); yield 1; } }
        function walkMap(m: Map<string, Item>) { for (const v of m.values()) bump(v); }";
    for body in [
        "for (const [k, v] of s.m) { const g = () => { v.v = 50; }; g(); }",
        "for (const [k, inner] of s.mm) { for (const [k2, v] of inner) bump(v); }",
        "const m = s.m; for (const [k, v] of m) bump(v);",
        "for (const [k, v] of s.m) { const w = v; w.v = 3; }",
        "keep(s.m.get(\"k1\")!);",
        "adopt(s, new Holder());",
        "f(s);",
        "walk(s);",
        "for (const x of gen(s)) {}",
        "walkMap(s.m);",
    ] {
        let r = err_src(&format!(
            "{defs} async function main() {{ const s = shared(new Store()); {body} }}"
        ));
        assert!(r.contains("read out of a `shared` value"), "{body}: {r}");
    }
    ok_src(&format!(
        "{defs} function sum(st: Store): number {{ let t = 0; for (const [k, v] of st.m) t += v.v; return t; }}
         function main() {{ const s = shared(new Store()); console.log(sum(s)); }}"
    ));
}

#[test]
fn a_mutex_cannot_become_any() {
    // `any` is read (printed, compared, written as JSON) without the lock.
    for to in ["any", "unknown", "object"] {
        let r = err_src(&format!(
            "class C {{ xs: number[] = [1]; }} function main() {{ const m = shared(new Mutex<C>(new C())); const o: {to} = m; }}"
        ));
        assert!(r.contains("it holds a lock"), "{to}: {r}");
    }
    ok_src(
        "class C { xs: number[] = [1]; } function main() { const m = shared(new Mutex<C>(new C())); const n: any = m.with((v) => v.xs.length); console.log(n); }",
    );
}

#[test]
fn values_derived_from_a_shared_read_cannot_be_changed() {
    // Whatever way a part travels (an assignment, a rebinding, a nested closure, a conditional,
    // an array literal, a method returning its own type, a constructor keeping it), a change
    // to it is refused; copying out (`slice()`) is explicit.
    let defs = "class Item { v: number = 3; }
        class Node { kids: Node[] = []; n: number = 1; first(): Node { return this.kids[0]; } }
        class H { i: Item; constructor(i: Item) { this.i = i; this.i.v = 99; } }
        class Store { m: Map<string, Item> = new Map<string, Item>(); items: Item[] = [new Item()]; root: Node = new Node(); }
        function bumpn(n: Node) { n.n += 100; }";
    for body in [
        "let w = new Item(); for (const [k, v] of s.m) { w = v; w.v = 50; }",
        "let y = new Item(); y = s.items[0]; y.v = 50;",
        "for (const [k, v] of s.m) { const g = () => { const h = () => { v.v = 7; }; h(); }; g(); }",
        "for (const [k, v] of s.m) { const w = k == \"k1\" ? v : v; w.v = 50; }",
        "for (const [k, v] of s.m) { const a = [v]; a[0].v = 50; }",
        "bumpn(s.root.first());",
        "const h = new H(s.items[0]);",
    ] {
        let r = err_src(&format!(
            "{defs} function main() {{ const s = shared(new Store()); {body} }}"
        ));
        assert!(r.contains("read out of a `shared` value"), "{body}: {r}");
    }
    for f in [
        "function f(st: Store) { let y = new Item(); y = st.items[0]; y.v = 50; }",
        "function f(st: Store) { for (const [k, v] of st.m) { const x = v; x.v = 5; } }",
        "function f(st: Store) { for (const [k, v] of st.m) { const g = () => { const h = () => { v.v = 7; }; h(); }; g(); } }",
    ] {
        let r = err_src(&format!(
            "{defs} {f} function main() {{ const s = shared(new Store()); f(s); }}"
        ));
        assert!(r.contains("read out of a `shared` value"), "{f}: {r}");
    }
    // Reading, copying out, and storing parts in a map or array of one's own are fine.
    ok_src(&format!(
        "{defs} function f(st: Store) {{ st.items.slice()[0] = new Item(); }}
         function g(st: Store): number {{ let t = 0; for (const x of st.items) {{ const y = x; t += y.v; }} return t; }}
         function main() {{ const s = shared(new Store()); f(s); console.log(g(s));
           const idx = new Map<string, Item>(); const list: Item[] = [];
           for (const [k, v] of s.m) {{ idx.set(k, v); list.push(v); }} console.log(idx.size, list.length); }}"
    ));
}

#[test]
fn a_lock_behind_a_type_parameter_is_not_read_without_it() {
    let r = err_src(
        "class C { xs: number[] = [1]; }
         function show<T>(x: T) { const o: any = x; console.log(o); }
         function show2<T>(x: T) { console.log(x, `${x}`); }
         function main() { const m = shared(new Mutex<C>(new C())); show(m); show2(m); }",
    );
    assert_eq!(r.matches("read here without its lock").count(), 2, "{r}");
    ok_src(
        "class C { xs: number[] = [1]; }
         function show<T>(x: T) { console.log(x, `${x}`); const o: any = x; }
         function main() { show(new C()); show(3); }",
    );
}

#[test]
fn new_containers_hold_parts_and_writes_through_results_are_refused() {
    let defs = "class Item { v: number = 3; name: string = \"a\"; tags: string[] = []; }
        class B { it: Item = new Item(); n: number = 0; }
        class Store { m: Map<string, Item> = new Map<string, Item>(); nums: Map<string, number> = new Map<string, number>();
          items: Item[] = [new Item()]; get head(): Item { return this.items[0]; } }";
    // A new container or literal holds parts: its own slots may change.
    ok_src(&format!(
        "{defs} function f(st: Store): number {{ const m = new Map<string, Item>(st.m); m.set(\"z\", new Item()); m.delete(\"a\"); return m.size; }}
         function top(m: Map<string, number>): number {{ const es = m.entries(); es.sort((a, b) => b[1] - a[1]); return es.length; }}
         function main() {{ const s = shared(new Store()); console.log(f(s), top(s.nums));
           const c = {{ ...s.items[0] }}; c.v = 50;
           const x = s.items[0]; const o = {{ name: x.name, tags: x.tags, n: 0 }}; o.n = 5;
           const pair: [Item, number] = [s.items[0], 1]; pair[1] = 2;
           const b = new B(); b.it = s.items[0]; b.n = 1; }}"
    ));
    // A change through it, or through what a method or getter returns, is refused.
    for body in [
        "s.m.get(\"k0\")!.v = 50;",
        "s.m.get(\"k0\")!.tags.push(\"x\");",
        "s.head.v = 50;",
        "s.head.tags.push(\"x\");",
        "const c = s.items.slice(); c[0].v = 50;",
        "const a: Item[] = [s.items[0]]; a[0].v = 50;",
        "const b = new B(); b.it = s.items[0]; b.it.tags.push(\"x\");",
    ] {
        let r = err_src(&format!(
            "{defs} function main() {{ const s = shared(new Store()); {body} }}"
        ));
        assert!(r.contains("read out of a `shared`"), "{body}: {r}");
    }
}

#[test]
fn parts_out_of_holders_and_inline_containers_cannot_be_changed() {
    let defs = "class Item { v: number = 3; tags: string[] = []; }
        class Bx { it: Item = new Item(); }
        class H2 { xs: Item[] = []; }
        class Store { m: Map<string, Item> = new Map<string, Item>(); items: Item[] = [new Item(), new Item()];
          get head(): Item { return this.items[0]; } pick(i: number): Item { return this.items[i]; } }";
    for body in [
        "(s.items.length > 0 ? s.head : s.pick(0)).v = 50;",
        "(s.items.length > 0 ? s.head : s.pick(0)).tags.push(\"x\");",
        "(s.items.length > 0 ? s.items[0] : s.items[1]).v = 50;",
        "s.items.slice()[0].v = 50;",
        "const xs = s.items; xs.slice()[0].v = 50;",
        "[s.items[0]][0].v = 50;",
        "s.m.values()[0].v = 50;",
        "const a = [s.items[0]]; const q = a[0]; q.v = 50;",
        "const b = new Bx(); b.it = s.items[0]; const q = b.it; q.v = 50;",
        "const b = new H2(); b.xs = s.items; const q = b.xs; q.push(new Item());",
        "const c = s.items.slice(); for (const x of c) { x.v = 50; }",
        "const c = s.items.slice(); c.forEach((x) => { x.v = 50; });",
        "[s.items[0]].forEach((x) => { x.v = 50; });",
        // Destructuring takes elements or fields out of the holder.
        "const a = [s.items[0]]; const [q] = a; q.v = 50;",
        "const b = { it: s.items[0] }; const { it } = b; it.v = 50;",
        "const [q] = s.items.slice(); q.v = 50;",
        // A loop over a literal, a conditional of a projection.
        "for (const x of [s.items[0]]) { x.v = 50; }",
        "const a = [s.items[0]]; const q = a.length > 0 ? a[0] : new Item(); q.v = 50;",
        // A callback that is a closure variable, nested callbacks, doubly nested literals.
        "const c = s.items.slice(); const g = (x: Item) => { x.v = 50; }; c.forEach(g);",
        "const c = s.items.slice(); c.forEach((x) => { [x].forEach((y) => { y.v = 50; }); });",
        "[[s.items[0]]][0][0].v = 50;",
        // A closure local that changes what it is given through another function value, or one
        // a nested closure assigns.
        "const g = (x: Item) => { const k = (y: Item) => { y.v = 50; }; k(x); }; const q = s.items[0]; g(q);",
        "const k = (y: Item) => { y.v = 50; }; const g = (x: Item) => { k(x); }; const q = s.items[0]; g(q);",
        "const g = (x: Item) => { const k = (t: string[]) => { t.push(\"x\"); }; k(x.tags); }; const q = s.items[0]; g(q);",
        "const k = (y: Item) => { y.v = 50; }; const g = (x: Item) => { k(x); }; const c = s.items.slice(); c.forEach(g);",
        "let g = (x: Item) => { console.log(x.v); }; const h = () => { g = (x: Item) => { x.v = 50; }; }; h(); const q = s.items[0]; g(q);",
        // A closure (whose own body reads no `shared`) changing a loop element it captures.
        "for (const x of s.items) { const g = () => { x.v = 50; }; g(); }",
        "for (const x of s.items) { const g = () => { x.tags.push(\"a\"); }; g(); }",
    ] {
        let r = err_src(&format!(
            "{defs} function main() {{ const s = shared(new Store()); {body} }}"
        ));
        assert!(r.contains("read out of a `shared`"), "{body}: {r}");
    }
    // One error per write, also through `??` and `!` (hidden locals).
    for body in [
        "(s.m.get(\"k0\") ?? new Item()).v = 50;",
        "(s.items.length == 1 ? s.m.get(\"k0\")! : s.m.get(\"k1\")!).v = 50;",
        "const o: { it: Item | undefined } = { it: s.items[0] }; (o.it ?? new Item()).v = 50;",
    ] {
        let r = err_src(&format!(
            "{defs} function main() {{ const s = shared(new Store()); {body} }}"
        ));
        assert_eq!(
            r.matches("read out of a `shared`").count(),
            1,
            "{body}: {r}"
        );
    }
    // A new container's own slots may change.
    ok_src(&format!(
        "{defs} function main() {{ const s = shared(new Store()); s.items.slice()[0] = new Item();
           const c = s.items.slice(); c.sort((a, b) => a.v - b.v); c.forEach((x) => {{ console.log(x.v); }});
           const a = [s.items[0]]; a[0] = new Item(); const q = a[0]; console.log(q.v); }}"
    ));
}

#[test]
fn fresh_objects_built_from_shared_numbers_or_by_map_may_change() {
    let defs = "class Item { v: number = 3; tags: string[] = []; name: string = \"n\";
          constructor(v: number) { this.v = v; } }
        class Store { items: Item[] = [new Item(1), new Item(2)]; get head(): Item { return this.items[0]; } }
        function mkc(x: Item): Item { const n = new Item(x.v); n.tags.push(\"z\"); return n; }";
    for body in [
        // `map` builds a new array of what its callback returns.
        "const ys = s.items.map((x) => new Item(x.v)); for (const y of ys) { y.v = 9; }",
        "const ys = s.items.map((x) => new Item(x.v)); ys.forEach((y) => { y.v = 9; });",
        "s.items.map((x) => new Item(x.v)).forEach((y) => { y.v = 9; });",
        "const ys = s.items.map((x) => new Item(x.v)).filter((y) => y.v > 0); ys.forEach((y) => { y.v = 9; });",
        "const out = s.items.map((x) => new Item(x.v)); out[0].v = 9;",
        // A reducer changes its accumulator, not the elements.
        "const init: number[] = []; [s.items[0]].reduce((a: number[], x: Item) => { a.push(x.v); return a; }, init);",
        // A number read out of the shared value holds no part of it.
        "const n = new Item(s.head.v); n.tags.push(\"q\");",
        "const o = { n: s.head.v, t: [] as string[] }; o.t.push(\"a\");",
        "const t: [number, string[]] = [s.head.v, []]; t[1].push(\"a\");",
        "s.items.map((x) => { const n = new Item(x.v); n.v += 10; n.tags.push(\"z\"); return n; });",
        "console.log(mkc(s.head).tags.length);",
        // `Array.from` with a mapper only reads.
        "console.log(Array.from(s.items, (x) => x.v).length);",
        "console.log(Array.from(s.items, (x) => x.name).length);",
        "const xs = s.items; console.log(Array.from(xs, (x) => x.v).length);",
        // A closure local that only reads what it is given.
        "const g = (x: Item) => { console.log(x.v); }; const q = s.items[0]; g(q);",
    ] {
        ok_src(&format!(
            "{defs} function main() {{ const s = shared(new Store()); {body} }}"
        ));
    }
}
