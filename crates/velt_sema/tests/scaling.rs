//! Semantic analysis scales linearly with program size. Programs are generated from
//! `bench/compile/unit.tmpl` (the compile-time benchmark's units: two classes, one overriding
//! the other's method, a struct implementing a shared interface, a function that modifies its
//! array param and one that calls down a chain of such functions), so every pass sees many
//! definitions, impls of one interface, vtables and call chains that modification inference
//! must propagate through. The fast test runs in CI; `cargo test -p velt_sema --release --test
//! scaling -- --ignored --nocapture` runs the N = 100 / 1000 / 5000 sizes. The instantiation
//! checks (record keys, JSON, promise copies), the type searches, the checks of arrows and of
//! expressions and the resolution of exports count their work (`check_work`), which these tests
//! compare between sizes exactly instead of by CPU time, which load on the machine tilts (#1502).
//! Only `sema_time_grows_linearly`, which guards every pass at once, measures CPU time.

mod common;

use std::sync::{Mutex, MutexGuard};

use common::hir_walk::func;
use common::process_work;
use common::programs::{err_src, load_src, load_src_at, ok_src, repo_root, Loaded};
use velt_sema::hir::PassMode;
use velt_sema::{check_work, instantiation_work, InstantiationWork};

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

/// A package of `n` modules behind barrels, as sigx's packages are: `index` re-exports every
/// module (`export *`), `left` and `right` re-export `index`, `pkg` re-exports both, and the root
/// imports every name of `pkg` by name. Each import once walked the barrels again and rebuilt
/// `pkg`'s whole export list.
fn barrel_package(n: usize) -> Loaded {
    let dir = std::env::temp_dir().join(format!("velt-barrels-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write =
        |name: &str, src: &str| std::fs::write(dir.join(format!("{name}.vlt")), src).unwrap();
    let mut index = String::new();
    for i in 0..n {
        write(
            &format!("m{i}"),
            &format!("export function f{i}(): i64 {{\n  return {i};\n}}\n"),
        );
        index += &format!("export * from \"./m{i}\";\n");
    }
    write("index", &index);
    write("left", "export * from \"./index\";\n");
    write("right", "export * from \"./index\";\n");
    write(
        "pkg",
        "export * from \"./left\";\nexport * from \"./right\";\n",
    );
    let names: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
    let mut main = format!("import {{ {} }} from \"./pkg\";\n", names.join(", "));
    main += "function main() {\n  let s: i64 = 0;\n";
    for i in 0..n {
        main += &format!("  s += f{i}();\n");
    }
    main += "  console.log(s);\n}\n";
    // The loader reads every module now: the directory is not needed after.
    let loaded = load_src_at(&dir.join("main.vlt"), &main);
    let _ = std::fs::remove_dir_all(&dir);
    loaded
}

/// The tests of this file run one at a time: `sema_cost` reads the process's CPU clock and the
/// work counters count every check in the process, which would count another test's work too.
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
    slopes_within(sizes, &best, slack)
}

/// The slopes of `cost` between consecutive `sizes`, each later one under `slack`× the first.
fn slopes_within(sizes: &[usize], cost: &[u64], slack: f64) -> Result<(), String> {
    let slopes: Vec<f64> = (1..sizes.len())
        .map(|i| (cost[i] as f64 - cost[i - 1] as f64).max(0.0) / (sizes[i] - sizes[i - 1]) as f64)
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

/// The work `counters` count during one `check` of `loaded`.
fn counted_work<const K: usize>(loaded: &Loaded, counters: &impl Fn() -> [u64; K]) -> [u64; K] {
    let start = counters();
    let (program, diags) = loaded.check();
    assert!(program.is_some(), "{}", loaded.render(&diags));
    let end = counters();
    std::array::from_fn(|i| end[i] - start[i])
}

/// The instantiation checks' counters.
fn instantiation_counters() -> [u64; 3] {
    let InstantiationWork {
        record_keys,
        json,
        promise_copies,
    } = instantiation_work();
    [record_keys, json, promise_copies]
}

/// Each instantiation check's work per unit (above an empty program's) must not grow by more
/// than `slack`× from `sizes[0]` to each larger size. The work is counted, so the comparison is
/// exact: a check that revisits every function per round of its fixed point, or scans every
/// impl per class type, grows by the size ratio per unit and fails.
fn assert_linear_work(sizes: &[usize], program: impl Fn(usize) -> String, slack: f64) {
    let names = ["record_keys", "json", "promise_copies"];
    let load = |n| load_src(&program(n));
    assert_linear_counted(sizes, load, names, instantiation_counters, slack);
}

/// [`assert_linear_work`] for the work `counters` count (named `names`) in the programs `load`
/// loads.
fn assert_linear_counted<const K: usize>(
    sizes: &[usize],
    load: impl Fn(usize) -> Loaded,
    names: [&str; K],
    counters: impl Fn() -> [u64; K],
    slack: f64,
) {
    let _serial = serial();
    let empty = counted_work(&load_src("function main() {}"), &counters);
    let per_unit: Vec<[f64; K]> = sizes
        .iter()
        .map(|&n| {
            let work = counted_work(&load(n), &counters);
            eprintln!("N = {n:>5}: work {work:?} (empty program {empty:?})");
            std::array::from_fn(|i| work[i].saturating_sub(empty[i]) as f64 / n as f64)
        })
        .collect();
    for (n, u) in sizes.iter().zip(&per_unit).skip(1) {
        for (i, name) in names.iter().enumerate() {
            assert!(
                u[i] <= slack * per_unit[0][i],
                "{name}: work per unit grew from {:.1} (N = {}) to {:.1} (N = {n})",
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
/// visit each type once, not each path (#859): `N` types with 2^N paths. The type searches stop
/// 32 levels down, so they are compared below that depth, where a walk by path costs 2^N: per
/// unit 2^8 / 8 at N = 8 against 2^20 / 20 at N = 20. They are compared first: a walk by path
/// does not finish at the record-key sizes.
#[test]
fn shared_type_graphs_are_walked_by_type() {
    let search = || [check_work().type_search];
    let load = |n| load_src(&shared_type_graph(n));
    assert_linear_counted(&[8, 20], load, ["type_search"], search, 1.5);
    assert_linear_work(&[100, 800], shared_type_graph, 1.5);
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
    let _serial = serial();
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
    let _serial = serial();
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

/// Importing every name of a package through barrels of `export *` costs the same per name
/// at every size: a module's exports are found once, not again for each import. Walking the
/// barrels again per imported name makes the work per module grow with the modules.
#[test]
fn imports_through_barrels_grow_linearly() {
    let exports = || [check_work().exports];
    assert_linear_counted(&[100, 800], barrel_package, ["exports"], exports, 1.5);
}

/// Choosing the member of a union of function types for nested arrows that each use the
/// enclosing arrow's parameter grows polynomially with the nesting, not exponentially (#859).
/// Each try of an arrow checks the arrows nested in it, so the arrows checked are quadratic in
/// the depth: their slope doubles with the depth, and a cubic one would quadruple.
#[test]
fn nested_arrow_trials_grow_polynomially() {
    let sizes = [15, 30, 60];
    let _serial = serial();
    let checks: Vec<u64> = sizes
        .iter()
        .map(|&n| {
            let [c] = counted_work(&load_src(&nested_trials(n)), &|| {
                [check_work().closure_checks]
            });
            eprintln!("depth {n:>3}: {c} arrows checked");
            c
        })
        .collect();
    if let Err(e) = slopes_within(&sizes, &checks, 3.0) {
        panic!("{e}");
    }
}

/// An array literal, an object literal and a call of a rest parameter, each holding one chain
/// of `n` additions (`[x + x + ..., ...ys]`), with spreads or (`spread` false) without.
fn spread_chains(n: usize, spread: bool) -> String {
    let chain = vec!["x"; n].join(" + ");
    let (array, object, call) = match spread {
        true => ("...ys", "...o", "...[1]"),
        false => ("1", "a: 1", "1"),
    };
    format!(
        "function g(...xs: number[]): number {{\n  return xs.length;\n}}\n\
         function main() {{\n  let x = 1;\n  const ys = [1];\n  const o = {{ a: 1, b: 2 }};\n  \
         console.log([{chain}, {array}]);\n  console.log({{ {object}, b: {chain} }}.b);\n  \
         console.log(g({chain}, {call}));\n  x = 2;\n  console.log(o.a, ys.length);\n}}\n"
    )
}

/// Whether an operand of a spread literal or call has effects, which decides what is evaluated
/// first, is told by one walk over it, so a long operand costs about what it costs without the
/// spread: telling it once checked the operand again per operator (800 terms took 19 s, 3000
/// over 300 s; `bench/spread_order/chains.sh`). Checking such a chain is not linear in its
/// length by itself, so the test compares the expressions checked with those of the same program
/// without spreads.
#[test]
fn spread_operand_chains_cost_what_they_cost_without_spreads() {
    let _serial = serial();
    let checks = |p: &Loaded| {
        let [c] = counted_work(p, &|| [check_work().expr_checks]);
        c
    };
    for n in [800, 3000] {
        let w = checks(&load_src(&spread_chains(n, true)));
        let wo = checks(&load_src(&spread_chains(n, false)));
        eprintln!("n = {n}: {w} expressions checked with spreads, {wo} without");
        assert!(
            (w as f64) < 1.5 * wo as f64,
            "n = {n}: {w} expressions checked with spreads against {wo} without"
        );
    }
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
