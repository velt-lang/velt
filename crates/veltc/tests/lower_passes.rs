//! The counted-types fixpoint's passes (velt_vir `lower_program`): the passes until the counted
//! types stop growing only find them and leave out the versions of view loops, and one more
//! pass lowers the program whole. Its VIR is the VIR of lowering every pass whole
//! (`VELT_LOWER_WHOLE`), and the view loops are versioned in that last pass only.

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
        "expected facts-only passes and a whole one:\n{err}"
    );
    let (last, earlier) = ps.split_last().unwrap();
    assert!(
        last.0 && last.1 > 0,
        "the last pass versions the loops:\n{err}"
    );
    for (whole, n) in earlier {
        assert!(!whole && *n == 0, "an earlier pass versioned loops:\n{err}");
    }
    let (all_whole, _) = lower(VIEWS, &[("VELT_LOWER_WHOLE", "1")]);
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
