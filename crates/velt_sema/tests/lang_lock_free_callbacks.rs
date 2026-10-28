//! Which `with` / `read` / `write` callbacks never reach a function's properties held as `any`
//! (locked_props.rs, `Program::lock_free_callbacks`): lowering takes the module lock before the
//! lock of every other one.

mod common;

use common::programs::ok_src;
use velt_sema::hir::{Def, Program};

const PRELUDE: &str = "
function decl(): number { return 1; }
class Box { n: number = 0; f: any = null; }
class Plain { n: number = 0; }
function readK(x: any): number { return x.k; }
function twice(n: number): number { return n * 2; }
function setup() { const d0: any = decl; d0.k = 1; }
";

/// Is every callback of a closure made in function `f` lock-free (`Some(true)`), every one not
/// (`Some(false)`)? `None` when `f` makes no closure.
fn lock_free(p: &Program, f: &str) -> Option<bool> {
    let prefix = format!("{f}::{{closure");
    let mut seen = None;
    for (i, d) in p.defs.iter().enumerate() {
        let Def::Fn(c) = d else { continue };
        if !c.name.starts_with(&prefix) {
            continue;
        }
        let free = p
            .lock_free_callbacks
            .contains(&velt_sema::hir::DefId(i as u32));
        match seen {
            None => seen = Some(free),
            Some(s) => assert_eq!(s, free, "closures of {f} differ"),
        }
    }
    seen
}

fn program(body: &str) -> Program {
    ok_src(&format!("{PRELUDE}\n{body}"))
}

#[test]
fn typed_callbacks_are_lock_free() {
    let p = program(
        "function typed(m: shared<Mutex<Plain>>, rw: shared<RwLock<Plain>>) {
           m.with((b) => { b.n += twice(1); });
           rw.read((b) => b.n + 1);
         }
         function main() { typed(shared(new Mutex(new Plain())), shared(new RwLock(new Plain()))); }",
    );
    assert_eq!(lock_free(&p, "typed"), Some(true));
}

#[test]
fn callbacks_reaching_any_members_are_not() {
    // Directly, through a field of the locked value, and through a function they call.
    for body in [
        "function f(m: shared<Mutex<Box>>, x: any) { m.with((b) => { b.n += x.k; }); }",
        "function f(m: shared<Mutex<Box>>, x: any) { m.with((b) => { b.n += b.f.k; }); }",
        "function f(m: shared<Mutex<Box>>, x: any) { m.with((b) => { b.n += readK(b.f); }); }",
        "function f(m: shared<Mutex<Box>>, x: any) { m.with((b) => { b.n += Object.keys(x).length; }); }",
        "function f(m: shared<Mutex<Box>>, x: any) { m.with((b) => { console.log(x); }); }",
    ] {
        let p = program(&format!(
            "{body}\nfunction main() {{ f(shared(new Mutex(new Box())), decl); }}"
        ));
        assert_eq!(lock_free(&p, "f"), Some(false), "{body}");
    }
}

#[test]
fn a_callback_holding_a_flagged_with_is_flagged() {
    let p = program(
        "function f(m: shared<Mutex<Box>>, n: shared<Mutex<Box>>, x: any) {
           m.with((b) => { n.with((c) => { c.n += x.k; }); b.n += 1; });
         }
         function main() { f(shared(new Mutex(new Box())), shared(new Mutex(new Box())), decl); }",
    );
    assert_eq!(lock_free(&p, "f"), Some(false));
}

#[test]
fn calling_a_function_value_is_not_lock_free() {
    let p = program(
        "function f(m: shared<Mutex<Box>>, g: () => number) { m.with((b) => { b.n += g(); }); }
         function main() { f(shared(new Mutex(new Box())), decl); }",
    );
    assert_eq!(lock_free(&p, "f"), Some(false));
}

/// Whether each closure made in function `f` is lock-free, in definition order.
fn each_lock_free(p: &Program, f: &str) -> Vec<bool> {
    let prefix = format!("{f}::{{closure");
    (p.defs.iter().enumerate())
        .filter(|(_, d)| matches!(d, Def::Fn(c) if c.name.starts_with(&prefix)))
        .map(|(i, _)| {
            p.lock_free_callbacks
                .contains(&velt_sema::hir::DefId(i as u32))
        })
        .collect()
}

#[test]
fn a_callback_holding_a_with_of_a_held_callback_is_not_lock_free() {
    // The inner callback is a value: lowering takes the module lock for it, so the typed outer
    // callback around it must hold it already.
    let p = program(
        "function f(m: shared<Mutex<Plain>>, n: shared<Mutex<Plain>>) {
           const inner = (y: Plain): number => y.n + 1;
           m.with((x) => n.with(inner));
         }
         function main() { f(shared(new Mutex(new Plain())), shared(new Mutex(new Plain()))); }",
    );
    // `inner` is no callback written at a `with` (never listed); the outer one is not listed.
    assert_eq!(each_lock_free(&p, "f"), vec![false, false]);
}

#[test]
fn a_proxy_held_as_a_typed_value_is_not_lock_free() {
    let p = program(
        "function f(m: shared<Mutex<Plain>>, q: { k: number }) { m.with((x) => { x.n += q.k; }); }
         function g(m: shared<Mutex<Plain>>, n: shared<Mutex<Plain>>) { m.with((x) => { x.n += 1; }); }
         function main() {
           const p: any = new Proxy({ k: 1 }, {});
           const q: { k: number } = p;
           f(shared(new Mutex(new Plain())), q);
           g(shared(new Mutex(new Plain())), shared(new Mutex(new Plain())));
         }",
    );
    assert_eq!(lock_free(&p, "f"), Some(false));
    assert_eq!(lock_free(&p, "g"), Some(true));
}

#[test]
fn generic_std_callbacks_are_lock_free() {
    // A promise's settlement runs `with` callbacks over its `T` in std: a type parameter is not
    // an `any`.
    let p = program(
        "async function main() { const n = await new Promise<number>((r) => r(1)); console.log(n); }",
    );
    assert_eq!(
        lock_free(&p, "std/prelude/promise::takeSettlement"),
        Some(true)
    );
}

#[test]
fn a_proxy_stored_through_any_into_a_typed_slot_is_not_lock_free() {
    // No conversion from `any` to `{ k: number }` in the program's own code: the push through
    // the `any` converts the proxy to the array's element type.
    let p = program(
        "function f(m: shared<Mutex<Plain>>, arr: { k: number }[]) { m.with((x) => { x.n += arr[0].k; }); }
         function main() {
           const arr: { k: number }[] = [];
           const aa: any = arr;
           aa.push(new Proxy({ k: 1 }, {}));
           f(shared(new Mutex(new Plain())), arr);
         }",
    );
    assert_eq!(lock_free(&p, "f"), Some(false));
}

#[test]
fn a_generic_class_nested_in_itself_reaches_its_inner_hooks() {
    // Dropping a `Box<number>` drops its `Box<Hooked>`, whose `Hooked` has a dispose hook that
    // writes a property: the walk by definition must not stop at the inner `Box`.
    let p = program(
        "class Hooked { [Symbol.dispose](): void { const e: any = decl; e.k2 = 1; } }
         class GBox<T> { v: T; other: GBox<Hooked> | null = null; constructor(v: T) { this.v = v; } }
         class GHolder { b: GBox<number> | null = null; }
         function f(m: shared<Mutex<Plain>>, h: GHolder) { m.with((x) => { h.b = null; }); }
         function main() { f(shared(new Mutex(new Plain())), new GHolder()); }",
    );
    assert_eq!(lock_free(&p, "f"), Some(false));
}
