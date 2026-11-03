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
