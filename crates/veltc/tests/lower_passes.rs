//! The counted-types fixpoint's passes (velt_vir `lower_program`): the passes until the counted
//! types stop growing only find them and leave out the versions of view loops, which are made
//! once the last pass is done (deferred_versions.rs), with no pass more. The VIR is the VIR of
//! lowering every pass whole (`VELT_LOWER_WHOLE`), and the view loops are versioned in the last
//! pass only.

mod no_window;
mod test_dir;

/// `velt build --emit vir` of `src` (as `main.vlt`) with `env` set: the VIR and stderr.
fn lower(src: &str, env: &[(&str, &str)]) -> (String, String) {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", "main.vlt", "--emit", "vir"])
        .current_dir(dir.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{err}");
    (String::from_utf8_lossy(&out.stdout).into_owned(), err)
}

/// Per pass of the profile: whether it was whole, and in how many functions it versioned view
/// loops.
fn passes(stderr: &str) -> Vec<(bool, usize)> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("velt-lower: pass "))
        .filter_map(|l| {
            let (_, rest) = l.split_once(' ')?;
            let whole = rest.starts_with("whole:");
            if !whole && !rest.starts_with("facts only:") {
                return None;
            }
            let n = rest
                .rsplit_once(" in ")?
                .1
                .split(' ')
                .next()?
                .parse()
                .ok()?;
            Some((whole, n))
        })
        .collect()
}

/// A view of an `any` (counted only after the first pass finds the conversion) and loops over
/// it and over a plain array, in a function and in `main`.
const VIEWS: &str = "
function sum(xs: number[]): number {
  let t = 0;
  for (let i = 0; i < xs.length; i++) {
    t += xs[i];
  }
  return t;
}

const raw: any = [1, 2, 3];
const v = raw as number[];
const plain: number[] = [4, 5, 6];
let n = 0;
for (const x of v) {
  n += x;
}
console.log(n, sum(v), sum(plain));
";

#[test]
fn view_loops_are_versioned_in_the_last_pass_only() {
    let (vir, err) = lower(VIEWS, &[("VELT_LOWER_PROFILE", "1")]);
    let ps = passes(&err);
    assert!(
        ps.len() >= 2,
        "expected passes that grow the counted types:\n{err}"
    );
    let (last, earlier) = ps.split_last().unwrap();
    assert!(
        !last.0 && last.1 > 0,
        "the last pass versions the loops:\n{err}"
    );
    for (whole, n) in earlier {
        assert!(!whole && *n == 0, "an earlier pass versioned loops:\n{err}");
    }
    let whole_env = [("VELT_LOWER_WHOLE", "1"), ("VELT_LOWER_PROFILE", "1")];
    let (all_whole, err_whole) = lower(VIEWS, &whole_env);
    let n_whole = passes(&err_whole).len();
    assert_eq!(ps.len(), n_whole, "a pass more:\n{err}\n{err_whole}");
    assert!(
        vir == all_whole,
        "the VIR differs from lowering every pass whole"
    );
}

#[test]
fn a_program_without_view_loops_is_lowered_as_before() {
    let src = "const xs: number[] = [1, 2];\nlet t = 0;\nfor (const x of xs) {\n  t += x;\n}\nconsole.log(t);\n";
    let (vir, err) = lower(src, &[("VELT_LOWER_PROFILE", "1")]);
    // No loop to version: the facts-only passes are the program, with no pass more.
    assert!(
        passes(&err).iter().all(|(whole, n)| !whole && *n == 0),
        "{err}"
    );
    let (all_whole, err_whole) = lower(
        src,
        &[("VELT_LOWER_WHOLE", "1"), ("VELT_LOWER_PROFILE", "1")],
    );
    assert_eq!(
        passes(&err).len(),
        passes(&err_whole).len(),
        "{err}\n{err_whole}"
    );
    assert!(
        vir == all_whole,
        "the VIR differs from lowering every pass whole"
    );
}

/// The terms the copies' dataflow kept in the last pass (`VELT_LOWER_PROFILE`).
fn terms_kept(stderr: &str) -> usize {
    let last = stderr
        .lines()
        .rfind(|l| l.contains(" functions, ") && l.ends_with(" terms kept"))
        .unwrap_or_else(|| panic!("no pass line:\n{stderr}"));
    let (_, rest) = last.rsplit_once(", ").unwrap();
    rest.split(' ').next().unwrap().parse().unwrap()
}

/// A view loop whose body computes `n` temporaries none of its tests reads.
fn long_loop(n: usize) -> String {
    let body: String = (0..n)
        .map(|k| format!("    t += x * {} + i * {};\n", k + 2, k + 1))
        .collect();
    format!(
        "function sum(xs: number[]): number {{\n  let t = 0;\n  for (let i = 0; i < xs.length; i++) {{\n    const x = xs[i];\n{body}  }}\n  return t;\n}}\n\nconst raw: any = [1, 2, 3];\nconst v = raw as number[];\nconsole.log(sum(v), sum([4, 5]));\n"
    )
}

#[test]
fn a_copy_keeps_only_the_terms_its_tests_need() {
    let env = [("VELT_LOWER_PROFILE", "1")];
    let (_, small) = lower(&long_loop(10), &env);
    let (_, large) = lower(&long_loop(200), &env);
    let (s, l) = (terms_kept(&small), terms_kept(&large));
    assert!(s > 0, "no terms kept:\n{small}");
    // Twenty times the temporaries in the same blocks: the terms the tests need stay as many
    // (keeping every once-assigned local's term grows them by a third on this program, most of
    // whose terms are the prelude's).
    assert!(
        l <= s + s / 10,
        "terms grew {l} / {s} for 20× the temporaries"
    );
}

/// The process's peak memory in MB by the end of lowering (`VELT_LOWER_PROFILE`, on Linux).
#[cfg(target_os = "linux")]
fn peak_mb(stderr: &str) -> usize {
    let last = stderr
        .lines()
        .rfind(|l| l.starts_with("velt-lower: peak "))
        .unwrap_or_else(|| panic!("no peak line:\n{stderr}"));
    last.split(' ').nth(2).unwrap().parse().unwrap()
}

/// A view loop reading an element through a field (a test that stays in the copy, so that it
/// hands over to the original) with `n` branches of its own.
#[cfg(target_os = "linux")]
fn branching_loop(n: usize) -> String {
    let body: String = (0..n)
        .map(|k| format!("    if (t > {}) {{\n      t -= {};\n    }}\n", k + 2, k + 1))
        .collect();
    format!(
        "class Holder {{\n  xs: number[];\n  constructor(xs: number[]) {{\n    this.xs = xs;\n  }}\n}}\n\nfunction sum(o: Holder, n: number): number {{\n  let t = 0;\n  for (let i = 0; i < n; i++) {{\n    t += o.xs[i];\n{body}  }}\n  return t;\n}}\n\nconst raw: any = [1, 2, 3];\nconst v = raw as number[];\nconsole.log(sum(new Holder(v), 3), sum(new Holder([4, 5, 6]), 3));\n"
    )
}

/// Versioning a long loop costs blocks × locals bits, and it comes when the rest of the program
/// is in memory (deferred_versions.rs). With a hash set per block this program peaks 183 MB above
/// the short loop; with the bits, under 10 MB.
#[cfg(target_os = "linux")]
#[test]
fn versioning_a_long_loop_does_not_raise_the_peak() {
    let env = [("VELT_LOWER_PROFILE", "1")];
    let (_, small) = lower(&branching_loop(100), &env);
    let (_, large) = lower(&branching_loop(2000), &env);
    let (s, l) = (peak_mb(&small), peak_mb(&large));
    assert!(l <= s + 100, "peak {l} MB against {s} MB");
}

/// Views of several element types, whose array-view helpers are shared by shape in debug
/// builds (velt_vir `lower/shape/`): their bodies are lowered in every pass and their facts
/// replayed per dictionary, so the passes find the same counted types as without sharing, and
/// the program is the one lowering every pass whole gives.
const SHARED_VIEWS: &str = "
type A = { a: number; b: string };
type B = { c: string; d: number };
type U = string | number;
class P {
  n: number;
  constructor(n: number) {
    this.n = n;
  }
}
const ra: any = [{ a: 1, b: \"x\" }];
const rb: any = [{ c: \"y\", d: 2 }];
const ru: any = [\"u\", 3];
const rp: any = [new P(1)];
const va = ra as A[];
const vb = rb as B[];
const vu = ru as U[];
const vp = rp as P[];
va.push({ a: 2, b: \"z\" });
vu[0] = 4;
console.log(va.length, vb[0].d, vu[0], vp[0].n, ra.length);
";

/// The growth lines of a lowering profile (`velt-lower: pass N grew: …`).
fn growth(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| l.contains(" grew: "))
        .map(str::to_string)
        .collect()
}

#[test]
fn shared_instances_keep_the_fixpoint() {
    let profile = [("VELT_LOWER_PROFILE", "1")];
    let (vir, shared) = lower(SHARED_VIEWS, &profile);
    // Of the pass that is the program: each pass prints the line, with 0 if nothing was shared.
    let last = (shared.lines())
        .rfind(|l| l.ends_with(" per type"))
        .unwrap_or_else(|| panic!("no count of shared instances:\n{shared}"));
    let count = last.rsplit_once(": ").unwrap().1.split(' ').next().unwrap();
    assert_ne!(count, "0", "the helpers were not shared:\n{shared}");
    let (_, plain) = lower(
        SHARED_VIEWS,
        &[("VELT_LOWER_PROFILE", "1"), ("VELT_SHAPES", "off")],
    );
    assert_eq!(
        growth(&shared),
        growth(&plain),
        "counted types grew differently"
    );
    assert_eq!(
        passes(&shared).len(),
        passes(&plain).len(),
        "another number of passes"
    );
    let (whole, _) = lower(SHARED_VIEWS, &[("VELT_LOWER_WHOLE", "1")]);
    assert!(vir == whole, "VIR differs from lowering every pass whole");
}
