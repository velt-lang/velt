//! Semantic analysis scales linearly with program size. Programs are generated from
//! `bench/compile/unit.tmpl` (the compile-time benchmark's units: two classes, one overriding
//! the other's method, a struct implementing a shared interface, a function that modifies its
//! array param and one that calls down a chain of such functions), so every pass sees many
//! definitions, impls of one interface, vtables and call chains that modification inference
//! must propagate through. The fast test runs in CI; `cargo test -p velt_sema --release --test
//! scaling -- --ignored --nocapture` runs the N = 100 / 1000 / 5000 sizes.

mod common;

use std::sync::{Mutex, MutexGuard};

use common::hir_walk::func;
use common::process_work;
use common::programs::{load_src, ok_src, repo_root, Loaded};
use velt_sema::hir::PassMode;

/// `n` generated units; `g{i}` calls `g{i-1}` except for every `chain`-th unit, which modifies
/// the array itself (so the modification travels up `chain` calls).
fn generated_program(n: usize, chain: usize) -> String {
    let path = repo_root().join("bench/compile/unit.tmpl");
    let unit = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n");
    let mut out = String::from("interface Scorer {\n  score(x: i64): i64;\n}\n\n");
    for i in 0..n {
        let link = match i % chain {
            0 => "  xs.push(1);\n  const r = 0;".to_string(),
            _ => format!("  const r = g{p}(xs, new Base{p}(0));", p = i - 1),
        };
        out += &unit
            .replace("@CHAIN@", &link)
            .replace("@I@", &i.to_string());
    }
    out += "function main() {\n  const xs: i64[] = [];\n";
    for i in 0..n {
        out += &format!("  f{i}(new Sub{i}({i}, 1.0), xs, \"s\");\n");
    }
    out + "}\n"
}

/// The tests of this file run one at a time: `sema_cost` reads the process's CPU clock, which
/// would count another test's work too.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The CPU work of one `check` (parsing excluded), in [`process_work`] units: CPU time, not
/// wall-clock time, which on a loaded machine mostly measures waiting for a core.
fn sema_cost(loaded: &Loaded) -> u64 {
    let start = process_work::now();
    let (program, diags) = loaded.check();
    let cost = process_work::now() - start;
    assert!(program.is_some(), "{}", loaded.render(&diags));
    cost
}

/// Sema cost per unit (above an empty program's) must not grow by more than `slack`× from
/// `sizes[0]` to each larger size. Sizes are measured interleaved, best of `ROUNDS` each, so
/// what load still shows in CPU time (cache and frequency effects of other processes) hits every
/// size alike instead of one of them.
fn assert_linear(sizes: &[usize], chain: impl Fn(usize) -> usize, slack: f64) {
    const ROUNDS: usize = 7;
    // Before loading: parsing the programs is work on the process's clock too.
    let _serial = serial();
    let mut programs = vec![load_src("function main() {}")];
    programs.extend(
        sizes
            .iter()
            .map(|&n| load_src(&generated_program(n, chain(n)))),
    );
    let mut best = vec![u64::MAX; programs.len()];
    for _ in 0..ROUNDS {
        for (b, p) in best.iter_mut().zip(&programs) {
            *b = (*b).min(sema_cost(p));
        }
    }
    let empty = best[0] as f64;
    eprintln!("empty program: cost {empty}");
    let per_unit: Vec<f64> = sizes
        .iter()
        .zip(&best[1..])
        .map(|(&n, &c)| {
            eprintln!("N = {n:>5} (chains of {:>5}): cost {c:>12}", chain(n));
            (c as f64 - empty).max(0.0) / n as f64
        })
        .collect();
    for (n, u) in sizes.iter().zip(&per_unit).skip(1) {
        assert!(
            *u < slack * per_unit[0],
            "sema cost per unit grew from {:.0} (N = {}) to {:.0} (N = {n})",
            per_unit[0],
            sizes[0],
            u,
        );
    }
}

/// One test, so the timings don't compete with each other for the CPU.
#[test]
fn sema_time_grows_linearly() {
    // 8× the units: quadratic growth would be 8× per unit; 4× leaves room for cache effects.
    // At least 100 units: the empty program's cost, subtracted from each, varies by about the
    // cost of 50 units between runs on a loaded machine.
    assert_linear(&[100, 800], |_| 8, 4.0);
    // One chain through the whole program: modification inference must not take a round over
    // every body per call level.
    assert_linear(&[100, 800], |n| n, 4.0);
}

#[test]
#[ignore = "slow: generates programs of up to 5000 units (run with --release)"]
fn sema_time_grows_linearly_large() {
    assert_linear(&[100, 1000, 5000], |_| 8, 2.0);
    assert_linear(&[100, 1000, 5000], |n| n, 2.0);
}

/// A modification 1500 calls down still reaches the top (the fixpoint once stopped after 1000
/// rounds, one call level each).
#[test]
fn modification_propagates_up_a_long_call_chain() {
    let _serial = serial();
    let mut src = String::from("function g0(xs: i64[]) {\n  xs.push(1);\n}\n");
    for i in 1..1500 {
        src += &format!("function g{i}(xs: i64[]) {{\n  g{}(xs);\n}}\n", i - 1);
    }
    src += "function main() {\n  const xs: i64[] = [];\n  g1499(xs);\n}\n";
    let p = ok_src(&src);
    assert_eq!(func(&p, "g1499").params[0].mode, PassMode::BorrowMut);
}
