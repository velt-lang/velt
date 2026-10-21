//! Semantic analysis scales linearly with program size. Programs are generated from
//! `bench/compile/unit.tmpl` (the compile-time benchmark's units: two classes, one overriding
//! the other's method, a struct implementing a shared interface, a function that modifies its
//! array param and one that calls down a chain of such functions), so every pass sees many
//! definitions, impls of one interface, vtables and call chains that modification inference
//! must propagate through. The fast test runs in CI; `cargo test -p velt_sema --release --test
//! scaling -- --ignored --nocapture` runs the N = 100 / 1000 / 5000 sizes. The instantiation
//! checks (record keys, JSON, promise copies) count their work, which these tests compare
//! between sizes exactly instead of by CPU time.

mod common;

use std::sync::{Mutex, MutexGuard};

use common::hir_walk::func;
use common::process_work;
use common::programs::{err_src, load_src, ok_src, repo_root, Loaded};
use velt_sema::hir::PassMode;
use velt_sema::{instantiation_work, InstantiationWork};

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

/// `n` generic functions, each calling the next one, so a requirement found in the last one
/// travels up the whole chain against definition order: a round over every function per call
/// level would be quadratic. The record-key and JSON requirements come from `JSON.parse` of a
/// `Record<K, i64>`, the promise-copy one from `xs.at(0)`, which copies a `T`.
fn generic_chains(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let (keys, first) = match i + 1 < n {
            true => (format!("k{}<K>(s)", i + 1), format!("p{}<T>(xs)", i + 1)),
            false => (
                "Object.keys(JSON.parse<Record<K, i64>>(s)).length".to_string(),
                "xs.at(0)".to_string(),
            ),
        };
        out += &format!(
            "function k{i}<K>(s: string): number {{\n  return {keys};\n}}\n\
             function p{i}<T>(xs: T[]): T | undefined {{\n  return {first};\n}}\n"
        );
    }
    out + "function main() {\n  k0<string>(\"{}\");\n  p0<i64>([1]);\n}\n"
}

/// A type graph whose types share parts: `N{k}` holds two `N{k-1}`, so `N{n-1}` has 2^n paths
/// to the `Record` in `N0`, while it has only `n` types. The record-key check walks the types a
/// function mentions, and reading `v` of the union `U` asks whether `N{n-1}` owns a resource.
fn shared_type_graph(n: usize) -> String {
    let mut out = String::from("interface N0 {\n  r: Record<string, number>;\n}\n");
    for k in 1..n {
        out += &format!("interface N{k} {{\n  a: N{p};\n  b: N{p};\n}}\n", p = k - 1);
    }
    let top = format!("N{}", n - 1);
    out += &format!("type U = {{ kind: \"a\"; v: {top} }} | {{ kind: \"b\"; v: {top} }};\n");
    out += &format!("function pick(u: U): {top} {{\n  return u.v;\n}}\n");
    out + &format!(
        "function main() {{\n  const v: {top} | null = null;\n  console.log(v === null);\n}}\n"
    )
}

/// Arrows nested `depth` deep, each passed where a union of function types is expected and
/// using the parameter of the arrow around it, so each try of an enclosing arrow gives the
/// nested one another type to see (#859).
fn nested_trials(depth: usize) -> String {
    let mut call = "1".to_string();
    for k in (1..=depth).rev() {
        call = format!(
            "run((x{k}) => {{\n  const u = x{p};\n  const r = {call};\n  const s: string = x{k};\n  return r;\n}})",
            p = k - 1
        );
    }
    "type H = ((x: number) => number) | ((x: string) => number);\n\
     function run(h: H): number {\n  return 1;\n}\n\
     function main() {\n  const x0 = \"s\";\n"
        .to_string()
        + &format!("  console.log({call});\n}}\n")
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

/// Sema cost per unit, measured as the slope between consecutive sizes, must not grow by more
/// than `slack`× from the first slope (`sizes[0]` to `sizes[1]`) to each later one. A slope
/// between two non-empty programs leaves out the cost every program pays (setting up, the
/// prelude), which is most of a small program's and once made the per-unit cost at the smallest
/// size mostly noise (#1339). Sizes are measured interleaved, best of `ROUNDS` each, so what load
/// still shows in CPU time (cache and frequency effects of other processes) hits every size alike
/// instead of one of them.
fn assert_linear(sizes: &[usize], chain: impl Fn(usize) -> usize, slack: f64) {
    assert_slopes(sizes, |n| generated_program(n, chain(n)), slack);
}

/// [`assert_linear`] for the programs `program` generates.
fn assert_slopes(sizes: &[usize], program: impl Fn(usize) -> String, slack: f64) {
    // A shared CI runner can slow one size's measurements enough to tilt a slope past the
    // slack. A pass that really grows faster than linearly fails every attempt, so the test
    // measures again, up to `ATTEMPTS` times, before it fails.
    const ATTEMPTS: usize = 3;
    assert!(
        sizes.len() >= 3,
        "ICE: assert_linear needs three sizes for two slopes"
    );
    // Before loading: parsing the programs is work on the process's clock too.
    let _serial = serial();
    let programs: Vec<Loaded> = sizes.iter().map(|&n| load_src(&program(n))).collect();
    let mut failure = String::new();
    for attempt in 1..=ATTEMPTS {
        match linear_slopes(sizes, &programs, slack) {
            Ok(()) => return,
            Err(e) => {
                eprintln!("attempt {attempt} of {ATTEMPTS}: {e}");
                failure = e;
            }
        }
    }
    panic!("{failure}");
}

/// One measurement for [`assert_linear`]: each size's best sema cost of `ROUNDS`, then the
/// slopes between consecutive sizes, each later one under `slack`× the first.
fn linear_slopes(sizes: &[usize], programs: &[Loaded], slack: f64) -> Result<(), String> {
    const ROUNDS: usize = 7;
    let mut best = vec![u64::MAX; programs.len()];
    for _ in 0..ROUNDS {
        for (b, p) in best.iter_mut().zip(programs) {
            *b = (*b).min(sema_cost(p));
        }
    }
    for (&n, &c) in sizes.iter().zip(&best) {
        eprintln!("N = {n:>5}: cost {c:>12}");
    }
    let slopes: Vec<f64> = (1..sizes.len())
        .map(|i| (best[i] as f64 - best[i - 1] as f64).max(0.0) / (sizes[i] - sizes[i - 1]) as f64)
        .collect();
    if slopes[0] <= 0.0 {
        return Err(format!(
            "sema cost did not grow from N = {} to {}: the measurement is broken",
            sizes[0], sizes[1],
        ));
    }
    for (i, s) in slopes.iter().enumerate().skip(1) {
        if *s >= slack * slopes[0] {
            return Err(format!(
                "sema cost per unit grew from {:.0} (N = {} to {}) to {:.0} (N = {} to {})",
                slopes[0],
                sizes[0],
                sizes[1],
                s,
                sizes[i],
                sizes[i + 1],
            ));
        }
    }
    Ok(())
}

/// The instantiation checks' work for one `check` of `loaded`.
fn instantiation_cost(loaded: &Loaded) -> [u64; 3] {
    let parts = |w: InstantiationWork| [w.record_keys, w.json, w.promise_copies];
    let start = parts(instantiation_work());
    let (program, diags) = loaded.check();
    assert!(program.is_some(), "{}", loaded.render(&diags));
    let end = parts(instantiation_work());
    [0, 1, 2].map(|i| end[i] - start[i])
}

/// Each instantiation check's work per unit (above an empty program's) must not grow by more
/// than `slack`× from `sizes[0]` to each larger size. The work is counted, so the comparison is
/// exact: a check that revisits every function per round of its fixed point, or scans every
/// impl per class type, grows by the size ratio per unit and fails.
fn assert_linear_work(sizes: &[usize], program: impl Fn(usize) -> String, slack: f64) {
    const PASSES: [&str; 3] = ["record_keys", "json", "promise_copies"];
    let _serial = serial();
    let empty = instantiation_cost(&load_src("function main() {}"));
    let per_unit: Vec<[f64; 3]> = sizes
        .iter()
        .map(|&n| {
            let work = instantiation_cost(&load_src(&program(n)));
            eprintln!("N = {n:>5}: work {work:?} (empty program {empty:?})");
            [0, 1, 2].map(|i| work[i].saturating_sub(empty[i]) as f64 / n as f64)
        })
        .collect();
    for (n, u) in sizes.iter().zip(&per_unit).skip(1) {
        for (i, pass) in PASSES.iter().enumerate() {
            assert!(
                u[i] <= slack * per_unit[0][i],
                "{pass}: work per unit grew from {:.1} (N = {}) to {:.1} (N = {n})",
                per_unit[0][i],
                sizes[0],
                u[i],
            );
        }
    }
}

#[test]
fn instantiation_work_grows_linearly() {
    // 8× the units: quadratic growth would be 8× per unit.
    assert_linear_work(&[100, 800], |n| generated_program(n, 8), 1.5);
    assert_linear_work(&[100, 800], |n| generated_program(n, n), 1.5);
    assert_linear_work(&[100, 800], generic_chains, 1.5);
}

/// One test, so the timings don't compete with each other for the CPU.
#[test]
fn sema_time_grows_linearly() {
    // The slope from 100 to 400 units against the one from 400 to 1600: purely quadratic growth
    // makes the second about 4× the first, and less when the quadratic part is small next to
    // linear work (an all-pairs loop over the prelude's functions as well as the program's). The
    // first slope spans 300 units, so it is well above the noise of two measurements; 2× leaves
    // room for cache effects.
    assert_linear(&[100, 400, 1600], |_| 8, 2.0);
    // One chain through the whole program: modification inference must not take a round over
    // every body per call level.
    assert_linear(&[100, 400, 1600], |n| n, 2.0);
}

/// Walks over a type graph whose types share parts (`known::owns_resource`, the record keys)
/// visit each type once, not each path (#859): `N` types with 2^N paths.
#[test]
fn shared_type_graphs_are_walked_by_type() {
    assert_linear_work(&[100, 800], shared_type_graph, 1.5);
    assert_slopes(&[200, 800, 3200], shared_type_graph, 2.0);
}

/// `n` interfaces `T{i}` and a type holding one of each, then `last`: a type far wider than a
/// walk over the type graph visits in a real program.
fn wide_holder(n: usize, generic: &str, last: &str) -> String {
    let mut out = String::new();
    for i in 0..n {
        out += &format!("interface T{i} {{\n  v: number;\n}}\n");
    }
    out += &format!("interface Holder{generic} {{\n");
    for i in 0..n {
        out += &format!("  p{i}: T{i};\n");
    }
    out + &format!("  {last};\n}}\n")
}

/// The walks over the type graph search a wide type completely: only depth (and the nesting of
/// generic types) limits them, not the number of types (#859).
#[test]
fn wide_types_are_searched_completely() {
    // The promise after 4100 other fields still makes reading `h` of the union a copy of a
    // value that owns one (`known::owns_resource`).
    let src = wide_holder(4100, "", "res: Promise<number>")
        + "interface A {\n  kind: \"a\";\n  h: Holder;\n}\n\
           interface B {\n  kind: \"b\";\n  h: Holder;\n}\n\
           function take(u: A | B): Holder {\n  return u.h;\n}\n\
           function main() {}\n";
    let e = err_src(&src);
    assert!(
        e.contains("cannot copy `h` of a union value `A | B`"),
        "{e}"
    );
    // The record after them still brings its key into `g`, which `main` instantiates with a
    // key type a record cannot have (`key_groups`).
    let src = wide_holder(4100, "<K>", "r: Record<K, number>")
        + "function g<K>(): number {\n  const h: Holder<K> | null = null;\n  return h === null ? 1 : 2;\n}\n\
           function main() {\n  console.log(g<number>());\n}\n";
    let e = err_src(&src);
    assert!(e.contains("`f64` cannot be a `Record` key"), "{e}");
    assert!(
        e.contains("required because `g` uses it as a `Record` key"),
        "{e}"
    );
}

/// A chain of `n` distinct generic interfaces, each holding the next in one of `wrappers`
/// (taken in turn), the last holding a `Record<K, number>`; `g` uses the first with `K` and
/// `main` instantiates it with `number`, which a record cannot have as a key.
fn wrapped_chain(n: usize, wrappers: &[&str]) -> String {
    let mut out: String = wrappers
        .iter()
        .map(|w| format!("interface {w}<T> {{\n  v: T;\n}}\n"))
        .collect();
    for i in 0..n {
        let w = wrappers[i % wrappers.len()];
        out += &format!("interface C{i}<K> {{\n  b: {w}<C{}<K>>;\n}}\n", i + 1);
    }
    out += &format!("interface C{n}<K> {{\n  r: Record<K, number>;\n}}\n");
    out + "function g<K>(): number {\n  const h: C0<K> | null = null;\n  return h === null ? 1 : 2;\n}\n\
           function main() {\n  console.log(g<number>());\n}\n"
}

/// The record-key walk follows a chain of distinct types through one generic wrapper (or two
/// in turn) to its end, however long: a wrapper whose type arguments don't grow is not a
/// generic type that grows as it recurses (#859).
#[test]
fn chains_through_generic_wrappers_are_walked_to_the_end() {
    for src in [
        wrapped_chain(70, &["Box"]),
        wrapped_chain(140, &["Box", "Cell"]),
    ] {
        let e = err_src(&src);
        assert!(e.contains("`f64` cannot be a `Record` key"), "{e}");
        assert!(
            e.contains("required because `g` uses it as a `Record` key"),
            "{e}"
        );
    }
}

/// Choosing the member of a union of function types for nested arrows that each use the
/// enclosing arrow's parameter grows polynomially with the nesting, not exponentially (#859).
/// Each try of an arrow checks the arrows nested in it, so the work is quadratic in the depth:
/// its slope doubles with the depth, and a cubic one would quadruple.
#[test]
fn nested_arrow_trials_grow_polynomially() {
    assert_slopes(&[15, 30, 60], nested_trials, 3.0);
}

#[test]
#[ignore = "slow: generates programs of up to 5000 units (run with --release)"]
fn sema_time_grows_linearly_large() {
    // Slopes 100 to 1000 against 1000 to 5000: purely quadratic growth makes the second about
    // 5.5×.
    assert_linear(&[100, 1000, 5000], |_| 8, 2.0);
    assert_linear(&[100, 1000, 5000], |n| n, 2.0);
    assert_linear_work(&[100, 1000, 5000], |n| generated_program(n, 8), 1.5);
    assert_linear_work(&[100, 1000, 5000], generic_chains, 1.5);
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
