//! A `with` callback written at the call takes along into the lock a variable only it uses
//! (ownership/locked/moves.rs): its capture moves, so what it stores into the value needs no
//! copy. Every other variable keeps its reference capture.

mod common;

use common::programs::ok_src;
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
