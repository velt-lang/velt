//! Drop elaboration: every test runs in the interpreter, which fails on leaks, double frees and
//! frees of non-heap pointers. The programs are Velt source loaded with the real std
//! (std_src.rs): their owned strings are numbers as text, which std's hooks make (#2197). Such a
//! text is inline (no heap buffer), so `heap(n)` appends to it, which gives it a buffer of its
//! own: one allocation, freed once, per call.

use super::run_src;
use super::{lower_src, user_fn};

/// `heap(n)`: `"<n> on the heap"` in a heap buffer of its own; `take(s)` takes `s` and drops it.
const HELPERS: &str = "function heap(n: i64): string {\n  let s = String(n);\n  \
                       s += \" on the heap\";\n  return s;\n}\n\n\
                       function take(s: string) {\n  const t = s;\n  console.log(\"took\", t);\n}\n\n";

/// `src` after the helpers.
fn program(src: &str) -> String {
    format!("{HELPERS}{src}")
}

fn has_flag(f: &crate::vir::Function) -> bool {
    f.locals
        .iter()
        .any(|l| l.name.as_deref().is_some_and(|n| n.ends_with(".dropflag")))
}

/// `let s = heap(12); if (c) take(s);`: `s` is dropped at the end only where it was not moved.
#[test]
fn conditional_move_uses_drop_flag() {
    let src = program(
        "function f(c: bool) {\n  const s = heap(12);\n  if (c) {\n    take(s);\n  }\n  \
         console.log(\"end\");\n}\n\nfunction main() {\n  f(true);\n  f(false);\n}\n",
    );
    let v = lower_src(&src);
    assert!(has_flag(user_fn(&v, "f")), "expected a drop flag:\n{v}");
    let out = run_src(&src);
    assert_eq!(out.stdout, "took 12 on the heap\nend\nend\n");
    assert_eq!(out.frees, 2);
}

/// Unconditional move: no flag, source not dropped, destination dropped once.
#[test]
fn unconditional_move_is_static() {
    let src =
        program("function main() {\n  const a = heap(1);\n  const b = a;\n  console.log(b);\n}\n");
    let v = lower_src(&src);
    assert!(!has_flag(user_fn(&v, "main")), "{v}");
    let out = run_src(&src);
    assert_eq!(out.stdout, "1 on the heap\n");
    assert_eq!(out.frees, 1);
}

/// Loop body locals are dropped on `break`, `continue` and normal iteration end.
#[test]
fn loop_exits_drop_body_locals() {
    let src = program(
        "function main() {\n  let i = 0;\n  while (true) {\n    const s = heap(i);\n    i += 1;\n    \
         if (i == 2) {\n      continue;\n    }\n    if (i == 4) {\n      break;\n    }\n    \
         console.log(s);\n  }\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "0 on the heap\n2 on the heap\n");
    assert_eq!(out.frees, 4);
}

/// Returning from nested scopes drops everything still owned; the returned value is moved out.
#[test]
fn return_from_nested_scope() {
    let src = program(
        "function g(n: i64): string {\n  const a = heap(n);\n  {\n    const b = a + \"x\";\n    \
         if (n > 1) {\n      return b;\n    }\n  }\n  return a;\n}\n\n\
         function main() {\n  console.log(g(1));\n  console.log(g(5));\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "1 on the heap\n5 on the heapx\n");
    // Two `heap` buffers and two concatenations, each freed once.
    assert_eq!(out.frees, 4);
}

/// Temporaries used only as borrows are dropped at the end of the statement.
#[test]
fn statement_temporaries_dropped() {
    let src = program(
        "function main() {\n  console.log(heap(1) + heap(2));\n  \
         console.log((heap(10) + \"\").length);\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "1 on the heap2 on the heap\n14\n");
    // Three `heap` buffers and two concatenations.
    assert_eq!(out.frees, 5);
}

/// Assignment drops the previous value (also `+=` on strings).
#[test]
fn assignment_drops_old_value() {
    let src = program(
        "function main() {\n  let s = heap(1);\n  s = heap(2);\n  s = s + \"x\";\n  \
         s += String(3);\n  console.log(s);\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "2 on the heapx3\n");
}

/// `let s; if (c) s = …;` — a conditionally initialized local gets a flag.
#[test]
fn uninit_let_conditional_assign() {
    let src = program(
        "function h(c: bool) {\n  let unused: string;\n  let s: string;\n  if (c) {\n    \
         s = heap(1);\n  }\n  console.log(\"h\");\n}\n\nfunction main() {\n  h(true);\n  h(false);\n}\n",
    );
    let v = lower_src(&src);
    assert!(has_flag(user_fn(&v, "h")), "{v}");
    let out = run_src(&src);
    assert_eq!(out.stdout, "h\nh\n");
    assert_eq!(out.frees, 1);
}

/// Owned params are dropped by the callee; the caller doesn't drop what it moved in.
#[test]
fn owned_param_dropped_by_callee() {
    let src = program(
        "function main() {\n  take(heap(7));\n  const s = heap(8);\n  take(s);\n  take(\"lit\");\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(
        out.stdout,
        "took 7 on the heap\ntook 8 on the heap\ntook lit\n"
    );
    assert_eq!(out.frees, 2);
}

/// Owned param moved on conditionally inside the callee.
#[test]
fn owned_param_conditionally_moved() {
    let src = program(
        "function maybe(s: string, c: bool) {\n  if (c) {\n    take(s);\n  }\n}\n\n\
         function main() {\n  maybe(heap(1), true);\n  maybe(heap(2), false);\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "took 1 on the heap\n");
    assert_eq!(out.frees, 2);
}

/// Value-producing `if` with owned branches, `&&` with temporaries in the rhs, block expressions.
#[test]
fn value_if_logical_and_block_exprs() {
    let src = program(
        "function main() {\n  const c = true;\n  const k = c ? heap(1) : heap(2);\n  \
         console.log(k, !c ? heap(3) : \"lit\");\n  console.log(c && heap(123).length == 15);\n  \
         {\n    const x = heap(9);\n    console.log(x + \"!\");\n  }\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "1 on the heap lit\ntrue\n9 on the heap!\n");
    // `heap(1)`, `heap(123)`, `heap(9)` and the concatenation.
    assert_eq!(out.frees, 4);
}

/// Moving a local inside a loop body declared outside: flagged; reassigned each iteration.
#[test]
fn move_in_loop_with_reassign() {
    let src = program(
        "function main() {\n  let s = heap(0);\n  for (let i = 0; i < 3; i++) {\n    \
         if (i == 1) {\n      take(s);\n      s = \"re\";\n    }\n    s = heap(i);\n  }\n  \
         console.log(s);\n}\n",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "took 0 on the heap\n2 on the heap\n");
    assert_eq!(out.frees, 4);
}

/// A function whose result is discarded: the returned string is still dropped.
#[test]
fn discarded_string_result_dropped() {
    let src = program("function main() {\n  heap(4);\n}\n");
    let out = run_src(&src);
    assert_eq!(out.frees, 1);
}
