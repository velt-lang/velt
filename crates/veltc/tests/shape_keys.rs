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
