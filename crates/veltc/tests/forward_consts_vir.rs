//! A closure written before the `const` it uses (#1142, sema `body/forward_consts.rs`): when
//! nothing between the closure and the declaration can run code, no closure can read the
//! constant before it is assigned, so the program has no ready flag and no `ReferenceError`
//! check; when something can (a call), it has both.

mod no_window;
mod test_dir;

/// Only closures are created between `f` and `g`'s declaration.
const INERT: &str = "function run(): number {
  const f = (): number => g() + 1;
  const g = (): number => 41;
  let s = 0;
  for (let i = 0; i < 3; i++) s += f();
  return s;
}
console.log(run());
";

/// `call(f)` may run `f` before `g` is assigned.
const CALLED: &str = "function call(h: () => number): number { return 0; }
function run(): number {
  const f = (): number => g() + 1;
  call(f);
  const g = (): number => 41;
  return f();
}
console.log(run());
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn no_ready_flag_when_nothing_can_run_before_the_declaration() {
    let v = vir(INERT);
    assert!(!v.contains("<ready g>"), "{v}");
    assert!(
        !v.contains("Cannot access 'g' before initialization"),
        "{v}"
    );
}

#[test]
fn a_ready_flag_when_a_call_may_run_the_closure_early() {
    let v = vir(CALLED);
    assert!(v.contains("<ready g>"), "{v}");
    assert!(v.contains("Cannot access 'g' before initialization"), "{v}");
}
