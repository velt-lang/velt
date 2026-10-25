//! A `with` callback written at the call takes along into the lock a variable only it uses
//! (ownership/locked/moves.rs): its capture moves, so what it stores into the value needs no
//! copy. Every other variable keeps its reference capture.

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, PassMode, Program};

/// The mode and `share` flag of variable `var`'s capture by a closure made in function `f`.
fn capture(p: &Program, f: &str, var: &str) -> (PassMode, bool) {
    let prefix = format!("{f}::{{closure");
    for d in &p.defs {
        let Def::Fn(c) = d else { continue };
        if !c.name.starts_with(&prefix) {
            continue;
        }
        for k in &c.captures {
            if c.body.locals[k.inner.0 as usize].name == var {
                return (k.mode, k.share);
            }
        }
    }
    panic!("no closure of {f} captures {var}");
}

fn moves(p: &Program, f: &str, var: &str) -> bool {
    capture(p, f, var) == (PassMode::Owned, false)
}

const PRELUDE: &str = "
class Item { n: i64 = 0; }
class Db { items: Item[] = []; }
function keep(d: Db, batch: Item[]) { for (const x of batch) { d.items.push(x); } }
";

fn program(body: &str) -> Program {
    ok_src(&format!("{PRELUDE}\n{body}"))
}

#[test]
fn a_variable_only_the_callback_uses_moves_into_the_lock() {
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           const batch = [new Item(), new Item()];
           m.with((d) => keep(d, batch));
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(moves(&p, "run", "batch"));
    // An owned parameter of an async function moves too.
    let p = program(
        "async function run(m: shared<Mutex<Db>>, batch: Item[]) {
           m.with((d) => keep(d, batch));
         }
         async function main() { await run(shared(new Mutex(new Db())), [new Item()]); }",
    );
    assert!(moves(&p, "run", "batch"));
}

#[test]
fn a_variable_used_later_keeps_its_reference() {
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           const batch = [new Item()];
           m.with((d) => { if (batch.length > d.items.length) { d.items.push(new Item()); } });
           console.log(batch.length);
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(!moves(&p, "run", "batch"));
}

#[test]
fn a_loop_around_the_call_must_declare_the_variable() {
    // Declared outside the loop: the next iteration uses it again.
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           const batch = [new Item()];
           for (let i = 0; i < 3; i++) { m.with((d) => keep(d, batch)); }
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(!moves(&p, "run", "batch"));
    // Declared in it: each iteration has its own.
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           for (let i = 0; i < 3; i++) {
             const batch = [new Item()];
             m.with((d) => keep(d, batch));
           }
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(moves(&p, "run", "batch"));
    // Declared in a `for` header: one variable for every iteration. This moved, and the next
    // iteration wrote to the locked value without the lock.
    let e = err_src(&format!(
        "{PRELUDE}
         function main() {{
           const m = shared(new Mutex(new Db()));
           for (let it = new Item(), i = 0; i < 3; i++) {{
             it.n = i;
             m.with((d) => {{ d.items.push(it); }});
           }}
         }}"
    ));
    assert!(e.contains("still used after `with` stored it"), "{e}");
}

#[test]
fn a_variable_another_closure_captures_keeps_its_reference() {
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           const batch = [new Item()];
           const size = (): i64 => batch.length as i64;
           m.with((d) => keep(d, batch));
           console.log(size());
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(!moves(&p, "run", "batch"));
}

#[test]
fn a_callback_making_a_closure_or_a_promise_keeps_its_captures() {
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           const batch = [new Item()];
           m.with((d) => batch.forEach((x) => d.items.push(x)));
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(!moves(&p, "run", "batch"));
}

#[test]
fn a_borrowed_parameter_and_a_string_keep_their_references() {
    let p = program(
        "function run(m: shared<Mutex<Db>>, batch: Item[]) {
           m.with((d) => keep(d, batch));
         }
         function main() { run(shared(new Mutex(new Db())), [new Item()]); }",
    );
    assert!(!moves(&p, "run", "batch"));
    let p = program(
        "function run(m: shared<Mutex<Db>>) {
           const name = `n${1}`;
           m.with((d) => { if (name.length > 5) { d.items.push(new Item()); } });
         }
         function main() { run(shared(new Mutex(new Db()))); }",
    );
    assert!(!moves(&p, "run", "name"));
}

#[test]
fn a_variable_declared_in_the_loop_is_not_used_by_the_next_iteration() {
    // Used before the `with` in its iteration: the next iteration has a new `batch`. This was
    // reported as "still used after `with` stored it".
    let p = program(
        "function run(m: shared<Mutex<Db>>, xs: Item[][]) {
           for (const x of xs) {
             const batch: Item[] = [];
             batch.push(new Item());
             m.with((d) => keep(d, batch));
           }
         }
         function main() { run(shared(new Mutex(new Db())), [[new Item()]]); }",
    );
    assert!(moves(&p, "run", "batch"));
}

#[test]
fn a_call_that_only_changes_bytes_of_an_argument_stores_nothing_in_it() {
    // `serve` changes the locked value and copies bytes of it into an outside buffer: a `u8[]`
    // holds no reference, so the call keeps nothing of the value. This was reported as "this
    // call may store a part of the locked value outside it, and also changes that argument".
    let p = program(
        "class Out { buf: u8[] = Buffer.alloc(64); length: i64 = 0;
           put(data: u8[]) { this.buf.set(data, this.length as usize); this.length += data.length as i64; } }
         class Store { values: u8[][] = [[1, 2, 3]]; reads: i64 = 0; }
         function serve(s: Store, out: Out) { s.reads += 1; out.put(s.values[0]); }
         function run(m: shared<Mutex<Store>>, out: Out) {
           m.with((s) => serve(s, out));
         }
         function main() { const out = new Out(); run(shared(new Mutex(new Store())), out); console.log(out.length); }",
    );
    let _ = p;
}

#[test]
fn an_rwlock_callback_keeps_its_captures() {
    // Only a `Mutex`'s `with` takes variables along: what an `RwLock`'s `read` or `write`
    // callback stores of a captured variable is that variable's own object (golden
    // lang/rwlock_read_caller_taken under wasm copied it when `m` moved).
    for call in ["read", "write"] {
        let p = program(&format!(
            "function run(rw: shared<RwLock<Db>>) {{
               const m = new Map<string, Item>();
               m.set(\"k\", new Item());
               const out: Item[] = [];
               rw.{call}((d) => {{ out.push(m.get(\"k\")!); return 0; }});
               console.log(out.length);
             }}
             function main() {{ run(shared(new RwLock(new Db()))); }}"
        ));
        assert!(!moves(&p, "run", "m"), "{call}");
    }
}

#[test]
fn a_store_through_a_union_or_a_base_class_counts() {
    // `put` stores `x` into a union / base-class argument whose own fields hold no reference;
    // the member or subclass can. Called through `wrap`, it stored `outside` into the locked
    // value with no copy.
    for (types, s) in [
        (
            "class A { item: Item = new Item(); } class B { k: i64 = 0; }",
            "A | B",
        ),
        (
            "class Base { k: i64 = 0; } class A extends Base { item: Item = new Item(); }",
            "Base",
        ),
    ] {
        let e = err_src(&format!(
            "{PRELUDE}
             {types}
             class Store {{ u: {s} = new A(); }}
             function put(s: {s}, x: Item) {{ if (s instanceof A) {{ s.item = x; }} }}
             function wrap(d: Store, x: Item) {{ put(d.u, x); }}
             function main() {{
               const m = shared(new Mutex(new Store()));
               const outside = new Item();
               m.with((d) => wrap(d, outside));
               outside.n = 42;
             }}"
        ));
        assert!(e.contains("still used after `with` stored it"), "{s}: {e}");
    }
    // A subclass that adds no reference does not make its base hold one: an overridden method
    // changing only numbers stores nothing.
    let _ = program(
        "class V { x: i64 = 1; }
         class Shape { x: i64 = 0; move(v: V) { this.x += v.x; } }
         class Circle extends Shape { r: i64 = 1; override move(v: V) { this.x += v.x * 2; } }
         class Holder { shape: Shape = new Circle(); }
         function main() {
           const m = shared(new Mutex(new Holder()));
           const v = new V();
           m.with((d) => { d.shape.move(v); });
           console.log(v.x, m.with((d) => d.shape.x));
         }",
    );
}
