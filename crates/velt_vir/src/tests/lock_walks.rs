//! The many-threads walk at a lock's release (glue/many.rs, async_fn/sync.rs `mutex_with`): a
//! callback that may store a function value into the lock's value is followed by a walk of the
//! value; one that stores none (`Program::store_free_callbacks`) is not, whatever the value
//! holds.

use super::{lower_src, run_src, user_fn};
use crate::vir::{self, Callee, Terminator};

/// A `Mutex` around an object holding an interface value and an array of function values, read
/// by one callback and written by others.
const SRC: &str = "interface Source {
  get(): number;
}

class Box implements Source {
  v: number = 1;
  get(): number {
    return this.v;
  }
}

class Leaf {
  src: Source = new Box();
  box: Box = new Box();
  fs: (() => number)[] = [];
  n: number = 0;
}

function read(m: shared<Mutex<Leaf>>): number {
  return m.with((l) => l.box.get() + l.src.get());
}

function count(m: shared<Mutex<Leaf>>) {
  m.with((l) => {
    l.n += 1;
  });
}

function store(m: shared<Mutex<Leaf>>, k: number) {
  m.with((l) => {
    l.fs.push(() => k);
  });
}

function main() {
  const m = shared(new Mutex(new Leaf()));
  count(m);
  store(m, 5);
  console.log(read(m), m.with((l) => l.n + l.fs.length));
}
";

/// Does function `f` call a many-threads check glue?
fn walks(v: &vir::Program, f: &vir::Function) -> bool {
    f.blocks.iter().any(|b| match &b.term {
        Terminator::Call {
            callee: Callee::Func(g),
            ..
        } => v.funcs[g.0 as usize].symbol.starts_with("_Gmanycheck"),
        _ => false,
    })
}

#[test]
fn a_release_after_a_callback_that_stores_no_function_value_does_not_walk() {
    let v = lower_src(SRC);
    assert!(!walks(&v, user_fn(&v, "read")), "`read` walks:\n{v}");
    assert!(!walks(&v, user_fn(&v, "count")), "`count` walks:\n{v}");
    assert!(
        walks(&v, user_fn(&v, "store")),
        "`store` does not walk:\n{v}"
    );
    assert_eq!(run_src(SRC).stdout, "2 2\n");
}
