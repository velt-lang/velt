//! Which `with` / `read` / `write` callbacks never store a value that may hold a function value
//! (module_state_stores.rs, `Program::store_free_callbacks`): the lock's release walks the value
//! for function values only after the others.

mod common;

use common::programs::ok_src;
use velt_sema::hir::{Def, DefId, Program};

const PRELUDE: &str = "
interface Source { get(): number; }
class Num implements Source { v: number = 1; get(): number { return this.v; } }
class Late implements Source {
  f: () => number = () => 2;
  get(): number { return 2; }
}
class Calls {
  f: () => number = () => 2;
  get(): number { return this.f(); }
}
class Plain { n: number = 0; }
class Holder {
  n: number = 0;
  src: Source = new Num();
  fs: (() => number)[] = [];
  f: (() => number) | null = null;
  plain: Plain | null = null;
  err: Base | null = null;
}
class Base { code: number = 0; }
class WithFn extends Base { f: () => number = () => 3; }
function twice(n: number): number { return n * 2; }
function keep(h: Holder, f: () => number) { h.f = f; }
";

/// Whether each closure made in function `f` is store-free, in definition order.
fn each_store_free(p: &Program, f: &str) -> Vec<bool> {
    let prefix = format!("{f}::{{closure");
    (p.defs.iter().enumerate())
        .filter(|(_, d)| matches!(d, Def::Fn(c) if c.name.starts_with(&prefix)))
        .map(|(i, _)| p.store_free_callbacks.contains(&DefId(i as u32)))
        .collect()
}

fn program(body: &str) -> Program {
    ok_src(&format!(
        "{PRELUDE}\n{body}\nfunction main() {{ f(shared(new Mutex(new Holder())), () => 1); }}"
    ))
}

#[test]
fn callbacks_storing_no_function_value_are_store_free() {
    for body in [
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.n += twice(1); }); }",
        // Through an interface: no implementation stores one.
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.n += h.src.get(); }); }",
        // A value of a class that holds none; reading a field that may hold one.
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.plain = new Plain(); }); }",
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.n += h.err == null ? 0 : h.err.code; }); }",
    ] {
        let p = program(body);
        assert_eq!(each_store_free(&p, "f"), vec![true], "{body}");
    }
}

#[test]
fn callbacks_that_may_store_a_function_value_are_not() {
    for body in [
        // Directly, into an array, through a call, as a value whose class holds one.
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.f = g; }); }",
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.fs.push(g); }); }",
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { keep(h, g); }); }",
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.src = new Late(); }); }",
        // A subclass of the field's class holds one.
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.err = new Base(); }); }",
        // A function value it calls may store anything, also one a method it calls calls.
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.n += g(); }); }",
        "function f(m: shared<Mutex<Holder>>, g: () => number) { m.with((h) => { h.n += new Calls().get(); }); }",
    ] {
        let p = program(body);
        assert_eq!(each_store_free(&p, "f"), vec![false], "{body}");
    }
}

#[test]
fn an_interface_whose_implementation_stores_one_is_followed() {
    let p = ok_src(&format!(
        "{PRELUDE}
         interface Sink {{ put(h: Holder): void; }}
         class Quiet implements Sink {{ put(h: Holder) {{ h.n += 1; }} }}
         class Loud implements Sink {{ put(h: Holder) {{ h.f = () => 4; }} }}
         function f(m: shared<Mutex<Holder>>, s: Sink) {{ m.with((h) => {{ s.put(h); }}); }}
         function main() {{ f(shared(new Mutex(new Holder())), new Quiet()); f(shared(new Mutex(new Holder())), new Loud()); }}"
    ));
    // The closure in `Loud.put` is made there, not at a `with`: the callback is the first one.
    assert_eq!(each_store_free(&p, "f"), vec![false]);
}

#[test]
fn a_generic_body_stores_what_its_type_arguments_hold() {
    let p = ok_src(&format!(
        "{PRELUDE}
         function put<T>(xs: T[], x: T) {{ xs.push(x); }}
         function putOn<U>(xs: U[], x: U) {{ put(xs, x); }}
         function seven(): number {{ return 7; }}
         function f(m: shared<Mutex<Holder>>, g: () => number) {{
           m.with((h) => {{ const ns: number[] = []; putOn(ns, 1); h.n += ns.length; }});
           m.with((h) => {{ put(h.fs, g); }});
           m.with((h) => {{ putOn(h.fs, seven); }});
         }}
         function main() {{ f(shared(new Mutex(new Holder())), () => 1); }}"
    ));
    // A type parameter passed on is resolved where the outer call names it.
    assert_eq!(each_store_free(&p, "f"), vec![true, false, false]);
}

#[test]
fn a_hook_a_drop_runs_is_followed() {
    // Dropping a `Keep` in the callback runs its `[Symbol.dispose]()`, which stores a function
    // (a `shared` value holds none for the analysis: making the `Keep` stores nothing).
    let p = ok_src(&format!(
        "{PRELUDE}
         class Keep {{
           h: shared<Mutex<Holder>>;
           constructor(h: shared<Mutex<Holder>>) {{ this.h = h; }}
           [Symbol.dispose]() {{ this.h.with((x) => {{ x.f = () => 5; }}); }}
         }}
         function f(m: shared<Mutex<Holder>>, g: () => number) {{
           m.with((h) => {{ h.n += 1; }});
           m.with((h) => {{ const k = new Keep(shared(new Mutex(new Holder()))); h.n += 1; }});
         }}
         function main() {{ f(shared(new Mutex(new Holder())), () => 1); }}"
    ));
    assert_eq!(each_store_free(&p, "f"), vec![true, false]);
}

#[test]
fn hooks_of_what_a_callback_returns_are_its_own() {
    // The result leaves the lock as a copy made under it, which runs `Part.clone()`: that stores
    // a function into the locked value.
    let p = ok_src(&format!(
        "{PRELUDE}
         class Part {{
           fs: (() => number)[] = [];
           n: number = 0;
           [Symbol.dispose]() {{}}
           clone(): Part {{ this.fs.push(() => 1); const p = new Part(); p.n = this.n; return p; }}
         }}
         class Box2 {{ part: Part = new Part(); n: number = 0; }}
         function f(m: shared<Mutex<Box2>>) {{
           m.with((b) => {{ b.n += 1; }});
           const p = m.with((b) => b.part);
           console.log(p.n);
         }}
         function main() {{ f(shared(new Mutex(new Box2()))); }}"
    ));
    assert_eq!(each_store_free(&p, "f"), vec![true, false]);
}
