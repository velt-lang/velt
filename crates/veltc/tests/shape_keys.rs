//! Generic instances shared by shape in debug builds (velt_vir `lower/shape/`,
//! docs/internals/design/shape-sharing.md): the end-of-pass check compares every answer the
//! shape key is built from, so a key missing a row is an ICE, not wrong code. Each test leaves
//! a row out of the key (`VELT_SHAPES_TEST_DROP`, debug compilers only) for a program whose
//! view element types differ in that row only, and expects the ICE; the same program built
//! normally runs.

mod no_window;
mod test_dir;

/// `velt run` of `src` (as `main.vlt`) with `env` set: (exit success, stdout, stderr).
fn run(src: &str, env: &[(&str, &str)]) -> (bool, String, String) {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["run", "main.vlt"]).current_dir(dir.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Views of two classes that differ only in being weak-capable (`P` is a `WeakRef` target):
/// their releases differ, so they must not share the helpers' code.
const WEAK: &str = "
class P {
  n: number;
  constructor(n: number) {
    this.n = n;
  }
}
class Q {
  m: number;
  constructor(m: number) {
    this.m = m;
  }
}
const p = new P(4);
const w = new WeakRef(p);
const rp: any = [p, new P(1)];
const rq: any = [new Q(2)];
const vp = rp as P[];
const vq = rq as Q[];
console.log(w.deref()?.n, vp[1].n, vq[0].m);
";

#[test]
fn a_weak_capable_class_has_a_shape_of_its_own() {
    let (ok, out, err) = run(WEAK, &[]);
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "4 1 2");
    if !cfg!(debug_assertions) {
        return;
    }
    let (ok, _, err) = run(WEAK, &[("VELT_SHAPES_TEST_DROP", "weak")]);
    assert!(
        !ok && err.contains("the shape key is missing a question"),
        "without the weak row the end-of-pass check must fire:\n{err}"
    );
}

/// Views of two recursive object types of one size: their own answers agree, but once one's
/// children are pushed onto through the view, `Tree[]` and `Tree2[]` don't
/// (tests/golden/lang/array_view_shapes_tree.vlt), so the key covers `T[]`.
const TREES: &str = "
type Tree = { v: number; kids: Tree[] };
type Tree2 = { w: string; kids: Tree2[] };
const t1: Tree = { v: 1, kids: [{ v: 2, kids: [] }] };
const t2: Tree2 = { w: \"a\", kids: [{ w: \"b\", kids: [] }] };
const r1: any = [t1];
const r2: any = [t2];
const v1 = r1 as Tree[];
const v2 = r2 as Tree2[];
console.log(v1[0].kids[0].v, v2[0].kids[0].w);
v1[0].kids.push({ v: 3, kids: [] });
console.log(v1[0].kids.length, r1[0].kids.length);
";

#[test]
fn the_key_covers_the_mirror_type() {
    let (ok, out, err) = run(TREES, &[]);
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "2 b\n2 2");
    if !cfg!(debug_assertions) {
        return;
    }
    let (ok, _, err) = run(TREES, &[("VELT_SHAPES_TEST_DROP", "array")]);
    assert!(
        !ok && err.contains("the shape key is missing a question"),
        "without `T[]` in the key the end-of-pass check must fire:\n{err}"
    );
}

/// A shared body that records a fact kept per function over a placeholder (boxing/tuples.rs)
/// is an ICE: one shared function stands for several instances there, so the fact can't be
/// replayed. The helpers record none; the test hook makes one record it.
#[test]
fn a_fact_per_function_in_a_shared_body_is_an_ice() {
    if !cfg!(debug_assertions) {
        return;
    }
    let (ok, _, err) = run(TREES, &[("VELT_SHAPES_TEST_DROP", "per-function")]);
    assert!(
        !ok && err.contains("a shared body's own facts per function are not replayed"),
        "the per-function guard must fire:\n{err}"
    );
}
