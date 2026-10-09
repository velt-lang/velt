//! M3/M4 semantic rules and their diagnostics: `await` placement, promise types, task safety,
//! shared state, throwing promises, JSON-serializable types, and argument move timing.

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn await_needs_an_async_body_and_a_promise() {
    let r = err_src(
        "async function f(): Promise<i64> { return 1; }\nfunction main() { const x = await f(); }",
    );
    assert!(
        r.contains("`await` is only allowed inside async functions"),
        "{r}"
    );
    let r = err_src(
        "async function f(): Promise<i64> { return 1; }
         async function main() { [1].forEach((x) => { await f(); }); }",
    );
    assert!(
        r.contains("`await` is only allowed inside async functions"),
        "{r}"
    );
    let r = err_src("async function main() { const x = await 1; }");
    assert!(r.contains("`await` needs a promise, found `i64`"), "{r}");
}

#[test]
fn async_signatures() {
    let r = err_src("async function f(): i64 { return 1; }\nasync function main() {}");
    assert!(r.contains("must be `Promise<T>`"), "{r}");
    assert!(r.contains("Promise<i64>"), "{r}");
    let r = err_src("async function f(): Promise<i64> { return \"s\"; }\nasync function main() {}");
    assert!(r.contains("mismatched types"), "{r}");
    ok_src("async function f() {}\nasync function main(): Promise<i32> { await f(); return 0; }");
}

#[test]
fn async_methods_own_this() {
    ok_src(
        "class C { n: i64 = 1; async get(): Promise<i64> { return this.n; } }
         async function main() { const c = new C(); console.log(await c.get()); }",
    );
    // Semantics stage 2: the receiver is shared with the promise when it is used again.
    ok_src(
        "class C { n: i64 = 1; async get(): Promise<i64> { return this.n; } }
         async function main() { const c = new C(); await c.get(); await c.get(); }",
    );
}

#[test]
fn tasks_cannot_mutate_captured_variables() {
    let r = err_src(
        "async function main() { let total = 0; const t = spawn(async () => { total += 1; }); await t; }",
    );
    assert!(r.contains("spawned task"), "{r}");
    assert!(r.contains("shared"), "{r}");
    let r = err_src(
        "async function main() { let n = 0; await spawn(async () => { [1, 2].forEach((x) => { n += x; }); }); }",
    );
    assert!(r.contains("modifies captured `n`"), "{r}");
    ok_src(
        "async function main() { const n = shared(0); const m = n.clone(); await spawn(async () => { m.add(1); }); console.log(n.get()); }",
    );
}

#[test]
fn spawn_takes_a_promise_or_a_task_body() {
    let r = err_src("async function main() { spawn(1); }");
    assert!(r.contains("mismatched types"), "{r}");
    let r = err_src("async function main() { spawn((x: i64) => x); }");
    assert!(
        r.contains("`spawn` needs a promise or an async task body"),
        "{r}"
    );
}

#[test]
fn shared_atomics_are_for_64_bit_integers() {
    ok_src("function main() { const s = shared(0); s.set(5); console.log(s.add(2), s.get()); }");
    let r = err_src("function main() { const s = shared(\"x\"); s.add(1); }");
    assert!(r.contains("no method named `add`"), "{r}");
}

#[test]
fn mutex_with_gets_the_value_mutably_and_is_synchronous() {
    ok_src(
        "function main() { const m = new Mutex<i64[]>([]); m.with((v) => { v.push(1); }); console.log(m.with((v) => v.length)); }",
    );
    let r = err_src(
        "async function main() { const m = new Mutex<i64[]>([]); m.with(async (v) => { v.push(1); }); }",
    );
    assert!(r.contains("cannot be async"), "{r}");
    // A Mutex is never Copy: a second name shares the same mutex (semantics stage 2).
    ok_src("function main() { const m = new Mutex<i64>(1); const k = m; console.log(m.with((v) => v), k.with((v) => v)); }");
}

#[test]
fn awaiting_any_promise_rethrows_its_typed_error() {
    let src = |body: &str| {
        format!(
            "class E {{ message: string = \"e\"; }}
             async function f(): Promise<i64> {{ throw new E(); }}
             async function main() {{ {body} }}"
        )
    };
    ok_src(&src(
        "try { await f(); } catch (e) { console.log(e.message); }",
    ));
    // A stored or spawned promise carries its error type; awaiting it rethrows.
    let p = ok_src(&src("const p = f(); await p; await spawn(f());"));
    assert!(common::hir_walk::func(&p, "main").throws.is_some());
    let p = ok_src(&src(
        "try { await Promise.all([f(), f()]); } catch (e) { console.log(e.message); }",
    ));
    assert!(common::hir_walk::func(&p, "main").throws.is_none());
    ok_src(&src("const p: Promise<i64, E> = f(); await p;"));
}

#[test]
fn json_types() {
    ok_src(
        "struct P { a: i64; b: string[]; c?: f64; }
         function main() { console.log(JSON.stringify(P { a: 1, b: [] })); const q = JSON.parse<P>(\"{}\"); console.log(q.a); }",
    );
    // Maps with string keys are objects; tuples are fixed-length arrays.
    ok_src(
        "type D = { m: Map<string, i64[]>, t: [string, f64] };
         function main() { const d = JSON.parse<D>(\"{}\"); console.log(JSON.stringify(d)); }",
    );
    let r = err_src(
        "function main() { const m = new Map<i64, i64>(); console.log(JSON.stringify(m)); }",
    );
    assert!(r.contains("`Map<i64, i64>` has no JSON form"), "{r}");
    assert!(r.contains("only with `string` keys"), "{r}");
    // Everything else without a JSON form is a diagnostic (never an ICE in lowering).
    for (decl, ty) in [
        ("interface Named { name(): string; }", "Named"),
        ("", "Promise<i64>"),
        ("", "shared<i64>"),
        ("", "() => void"),
    ] {
        let r = err_src(&format!(
            "{decl} type W = {{ f: {ty} }};
             async function main() {{ const w = JSON.parse<W>(\"{{}}\"); }}"
        ));
        assert!(
            r.contains(&format!("contains `{ty}`, which has no JSON form")),
            "{r}"
        );
    }
    // An unknown type is reported once, not again as having no JSON form.
    let r = err_src("type W = { s: Nope }; function main() { JSON.parse<W>(\"{}\"); }");
    assert!(!r.contains("JSON"), "{r}");
    let r = err_src(
        "struct W { f: (x: i64) => i64; }
         function main() { const w = JSON.parse<W>(\"{}\"); }",
    );
    assert!(r.contains("cannot convert to or from JSON"), "{r}");
    let r = err_src(
        "function send<T>(v: T): string { return JSON.stringify(v); }
         function main() { console.log(send(shared(1))); }",
    );
    assert!(r.contains("`shared<i64>` has no JSON form"), "{r}");
}

#[test]
fn moves_into_call_arguments_happen_at_the_call() {
    ok_src(
        "function main() {
           const counts = new Map<string, i64>();
           const word = \"a\";
           counts.set(word, (counts.get(word) ?? 0) + 1);
           console.log(counts.size);
         }",
    );
    ok_src(
        "struct P { s: string; n: usize; }
         function main() { const s = \"ab\"; const p = P { s: s, n: s.length }; console.log(p.n); }",
    );
    // Semantics stage 2: the first argument shares the array.
    ok_src(
        "function keep(a: i64[], b: i64[]): i64[][] { return [a, b]; }
         function main() { const s = [1]; console.log(keep(s, s)); }",
    );
    ok_src(
        "function keep(a: i64[]): i64[][] { return [a]; }
         function both(a: i64[][], b: i64[][]): usize { return a.length + b.length; }
         function main() { const s = [1]; console.log(both(keep(s), keep(s))); }",
    );
    // The same with strings: the first argument is a copy.
    ok_src(
        "function keep(a: string, b: string): string[] { return [a, b]; }
         function main() { const s = \"x\"; console.log(keep(s, s)); }",
    );
}

#[test]
fn generic_structs_cannot_hold_void() {
    let r = err_src(
        "struct B<T> { v: T; }
         function f() {}
         function main() { const b = B { v: f() }; }",
    );
    assert!(r.contains("would have a field `v` of type `void`"), "{r}");
}

#[test]
fn values_owning_resources_have_no_automatic_clone() {
    let r = err_src(
        "class H { h: u64 = 1; [Symbol.dispose]() {} }
         function main() { const a = new H(); const b = a.clone(); }",
    );
    assert!(r.contains("has no automatic `clone()`"), "{r}");
}

#[test]
fn with_summaries_reach_through_call_chains_declared_callers_first() {
    // Each function is declared before the one it calls, and `even` and `odd` call each other:
    // what `last` does (start a promise, store into its other argument) only reaches `first`
    // after several rounds of the summaries' fixpoint, each re-summarizing only the callers of
    // what changed.
    let src = |body: &str| {
        format!(
            "class Inner {{ n: i64 = 0; }}
             class Outer {{ n: i64 = 0; inner: Inner = new Inner(); }}
             function first(o: Outer, xs: Inner[]) {{ even(o, xs, 4); }}
             function even(o: Outer, xs: Inner[], k: i64) {{ if (k > 0) {{ odd(o, xs, k - 1); }} else {{ middle(o, xs); }} }}
             function odd(o: Outer, xs: Inner[], k: i64) {{ even(o, xs, k - 1); }}
             function middle(o: Outer, xs: Inner[]) {{ last(o, xs); }}
             function last(o: Outer, xs: Inner[]) {{ {body} }}
             async function bump(o: Outer): Promise<i64> {{ await yieldNow(); o.n += 1; return o.n; }}
             function main() {{
               const m = new Mutex<Outer>(new Outer());
               const out: Inner[] = [];
               m.with((v) => first(v, out));
               m.with((v) => {{ const ys: Inner[] = []; first(v, ys); }});
             }}"
        )
    };
    ok_src(&src("o.n += 1;"));
    let r = err_src(&src("const p = bump(o);"));
    assert!(
        r.contains("`first` starts a promise with the locked value"),
        "{r}"
    );
    // A store from the value into the outside array is transferred (copied), not an error.
    ok_src(&src("xs.push(o.inner);"));
    let r = err_src(&src("o.n += 1; xs.push(o.inner);"));
    assert!(r.contains("also changes that argument"), "{r}");
}

#[test]
fn with_summaries_follow_a_self_recursive_call_that_swaps_its_arguments() {
    // `sw` makes a promise from its first parameter directly, and from its second only through
    // the recursive call that swaps them: that needs `sw` summarized again after its own
    // summary changed.
    let src = |call: &str| {
        format!(
            "class Outer {{ n: i64 = 0; }}
             async function bump(o: Outer): Promise<i64> {{ await yieldNow(); o.n += 1; return o.n; }}
             function sw(a: Outer, b: Outer, k: i64) {{
               if (k > 0) {{ sw(b, a, k - 1); }} else {{ const p = bump(a); }}
             }}
             function main() {{
               const m = new Mutex<Outer>(new Outer());
               const other = new Outer();
               m.with((v) => {call});
             }}"
        )
    };
    for call in ["sw(v, other, 0)", "sw(other, v, 1)"] {
        let r = err_src(&src(call));
        assert!(
            r.contains("`sw` starts a promise with the locked value"),
            "{call}: {r}"
        );
    }
}
