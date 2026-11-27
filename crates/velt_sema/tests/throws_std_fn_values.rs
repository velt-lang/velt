//! A call through a parameter of an arrow passed to std, typed there by a function type written in
//! std (`new Promise`'s `resolve` and `reject`), throws what that type says: not what the user's
//! function types of the same shape hold (`throws::std_values`, #3949). A parameter the arrow
//! assigns, or a closure in it does, may hold such a user value, and its calls still throw it.

mod common;

use common::programs::{err_src, ok_src};

/// A `(n: number) => void` variable holding a throwing closure, which a `then` callback calls,
/// and executors whose `resolve` has that type; `$BODY` is the executor's body.
const PROGRAM: &str = r#"
class Negative extends Error {}
class Refused extends Error {}

let open: (n: number) => void = (n: number) => {};

function later(n: number): Promise<number> {
  return new Promise((r, reject) => {
    if (n < 0) {
      reject(new Negative("negative"));
      return;
    }
    $BODY
  });
}

async function main(): Promise<void> {
  open = (n: number) => {
    throw new Refused("refused");
  };
  console.log(await later(1));
  try {
    await Promise.resolve().then(() => open(1));
  } catch (e) {
    console.log("caught", e instanceof Refused);
  }
}

main();
"#;

fn program(body: &str) -> String {
    PROGRAM.replace("$BODY", body)
}

#[test]
fn an_executors_resolve_throws_nothing_beside_a_user_value_of_its_type() {
    ok_src(&program("r(n);"));
    // Called in a callback in the executor, and passed on as a value.
    ok_src(&program("queueMicrotask(() => r(n));"));
    ok_src(&program("const k = r;\n    k(n);"));
}

#[test]
fn an_assigned_executor_parameter_throws_what_its_values_may() {
    // `r` holds `open` when it is called: its call throws `Refused`, which the executor's
    // `Negative` does not allow.
    let e = err_src(&program("r = open;\n    r(n);"));
    assert!(e.contains("throws `Refused`"), "{e}");
    let e = err_src(&program(
        "const swap = () => {\n      r = open;\n    };\n    swap();\n    r(n);",
    ));
    assert!(e.contains("throws `Refused`"), "{e}");
}
