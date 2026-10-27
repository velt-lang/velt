//! A module constant made by `new Atomic<T>(literal)` is a word in the program's data (velt_vir
//! `lower/static_atomic.rs`): its uses reach no module state, so they never take the module lock
//! (rt_abi.md `velt_rt_module_state`), which would serialize every task that uses the atomic.

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.vlt`.
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

const BODY: &str = "
const HITS = new Atomic<u64>(INIT);

async function work() {
  HITS.add(1, \"relaxed\");
}

async function main() {
  await spawn(work());
  console.log(HITS.load());
}
";

#[test]
fn a_literal_starting_value_needs_no_module_state() {
    let v = vir(&BODY.replace("INIT", "0"));
    assert!(!v.contains("velt_rt_module_state"), "{v}");
    assert!(v.contains("velt_atomic_add_64_rlx"), "{v}");
}

#[test]
fn a_computed_starting_value_is_kept_in_the_module_state() {
    let src = format!(
        "function start(): u64 {{ return 2; }}\n{}",
        BODY.replace("INIT", "start()")
    );
    let v = vir(&src);
    assert!(v.contains("velt_rt_module_state"), "{v}");
}
