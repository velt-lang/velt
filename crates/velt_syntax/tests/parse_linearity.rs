//! Parse cost grows linearly with input size where the parser, not the lexer, decides what the
//! input is (JSX elements, generic arrows): parsing `make(4 * n)` costs about 4 times as much as
//! `make(n)`, not 16 times.
//!
//! Cost is the process's CPU time ([`process_work`]): `parse_file` parses on a thread of its own,
//! and wall-clock time on a loaded machine mostly measures waiting for a core. A test binary of
//! its own, with its tests serialized, so no other test's work lands on the same clock.

mod common;
#[path = "common/process_work.rs"]
mod process_work;

use std::sync::{Mutex, MutexGuard};

use common::*;

/// The tests of this file run one at a time: the process's CPU clock would count another test's
/// work too.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Asserts that `make(4 * n)` costs less than 9 times `make(n)`. The best of several interleaved
/// runs of each size evens out what load still shows in CPU time (cache and frequency effects of
/// other processes).
fn assert_linear(what: &str, n: usize, make: impl Fn(usize) -> String) {
    const ROUNDS: usize = 7;
    let (small, large) = (make(n), make(4 * n));
    let _serial = serial();
    let cost = |src: &str| {
        process_work::measure(|| {
            let _ = std::hint::black_box(parse(src));
        })
    };
    let (mut c_small, mut c_large) = (u64::MAX, u64::MAX);
    for _ in 0..ROUNDS {
        c_small = c_small.min(cost(&small));
        c_large = c_large.min(cost(&large));
    }
    assert!(
        c_large < c_small.saturating_mul(9),
        "{what}: {n} units cost {c_small}, {} cost {c_large}: not linear",
        4 * n
    );
}

/// Nested elements in containers must not re-lex the rest of the file each time (#218).
#[test]
fn nested_elements_and_generics() {
    assert_linear("nested elements and generics", 5_000, |n| {
        JSX_UNIT.repeat(n)
    });
}

/// Each re-lex drops the lookahead caches past its `<`, not the ones for the whole file.
#[test]
fn parentheses_before_elements() {
    assert_linear("parentheses before elements", 5_000, |n| {
        let parens = "function f(a: i64): i64 { return ((a + (1)) * (a - (2))) / (a + (3)); }\n";
        parens.repeat(n) + &"const p = <p>{(1)}</p>;\n".repeat(n)
    });
}

/// ... and the lexer's diagnostics past it, not all of them.
#[test]
fn lexer_errors_before_elements() {
    assert_linear("lexer errors before elements", 10_000, |n| {
        "const x = 1 \u{a7}; const y = <p>a</p>;\n".repeat(n)
    });
}
